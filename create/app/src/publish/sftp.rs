use super::{
    config::Target,
    field, invalid, nonce,
    post::{available_folder, part_folder, utc, Post},
    store::Store,
    Scratch,
};
use makepad_work_core::home::write_private;
use makepad_strict_json::{obj, parse, s, Value};
use makepad_widgets::makepad_platform::thread::{Class, SubmitError, TaskHandle, TaskPool};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{mpsc::Sender, Arc, Mutex},
    time::SystemTime,
};

#[derive(Debug)]
pub struct Progress {
    pub file: String,
    /// Media bytes known to be present, including completed files from an earlier attempt.
    pub bytes: u64,
    pub total: u64,
}

#[derive(Clone, Debug)]
pub struct Published {
    pub folder: String,
    pub url: Option<String>,
}

#[derive(Default)]
struct Attempt {
    part: String,
    folder: String,
    verified: bool,
}

/// One confirmed post and its retry state. Retain this object after a failed attempt.
pub struct Upload {
    store: Store,
    target: Target,
    post: Post,
    ids: Vec<String>,
    token: String,
    attempt: Mutex<Attempt>,
}

fn quote(path: &str) -> io::Result<String> {
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err(invalid("sftp needs a single-line path"));
    }
    // Inside double quotes sftp protects glob characters itself. Escape the quote
    // and the backslash that would otherwise change how the next character is read.
    Ok(format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\"")))
}

fn relative(name: &str) -> io::Result<String> {
    quote(&format!("./{name}"))
}

fn argv(target: &Target) -> io::Result<Vec<&str>> {
    target.validate()?;
    Ok(vec!["-b", "-", "-o", "BatchMode=yes", &target.ssh])
}

struct Batch<'a> {
    program: &'a Path,
    target: &'a Target,
    scratch: &'a Path,
}

impl Batch<'_> {
    fn cd(&self) -> io::Result<String> {
        let dir = &self.target.dir;
        let path = if dir.starts_with('-') { relative(dir)? } else { quote(dir)? };
        Ok(format!("@cd {path}\n"))
    }

    fn run(&self, script: &str, mut line: impl FnMut(&str)) -> io::Result<()> {
        // OpenSSH reads batch input into a 2048-byte buffer. Never let it split a
        // pathname across commands. Both the newline and terminating NUL must fit.
        if script.lines().any(|line| line.len() > 2046) {
            return Err(invalid("path exceeds the sftp batch line length"));
        }
        let input = self.scratch.join("batch");
        let errors = self.scratch.join("errors");
        fs::write(&input, script)?;
        let mut child = Command::new(self.program)
            .args(argv(self.target)?)
            .current_dir(self.scratch)
            .stdin(File::open(input)?)
            .stdout(Stdio::piped())
            .stderr(File::create(&errors)?)
            .spawn()?;
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut buf = Vec::new();
        let read = loop {
            buf.clear();
            match stdout.read_until(b'\n', &mut buf) {
                Ok(0) => break Ok(()),
                Ok(_) => line(String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n'])),
                Err(e) => break Err(e),
            }
        };
        drop(stdout);
        let status = child.wait()?;
        if !status.success() {
            let diagnostic = fs::read(errors)?;
            return Err(io::Error::other(format!("sftp {status}: {}", String::from_utf8_lossy(&diagnostic).trim())));
        }
        read
    }

    fn names(&self, folder: Option<&str>) -> io::Result<HashSet<String>> {
        let mut script = self.cd()?;
        if let Some(folder) = folder {
            script.push_str(&format!("@cd {}\n", relative(folder)?));
        }
        script.push_str("@ls -1a \".\"\n");
        let mut names = HashSet::new();
        self.run(&script, |line| {
            names.insert(line.strip_prefix("./").unwrap_or(line).to_owned());
        })?;
        Ok(names)
    }

    fn sizes(&self, part: &str, post: &Post) -> io::Result<HashMap<String, u64>> {
        let names = self.names(Some(part))?;
        let mut script = self.cd()?;
        script.push_str(&format!("@cd {}\n", relative(part)?));
        let existing: Vec<_> = post.media.iter().filter(|m| names.contains(&m.file)).collect();
        if existing.is_empty() {
            return Ok(HashMap::new());
        }
        for media in &existing {
            script.push_str(&format!("@ls -ln {}\n", relative(&media.file)?));
        }
        let mut sizes = Vec::new();
        self.run(&script, |line| {
            sizes.push(line.starts_with('-').then(|| line.split_ascii_whitespace().nth(4)?.parse::<u64>().ok()).flatten());
        })?;
        if sizes.len() != existing.len() {
            return Err(invalid("could not read staged file sizes"));
        }
        existing
            .into_iter()
            .zip(sizes)
            .map(|(m, size)| Ok((m.file.clone(), size.ok_or_else(|| invalid("invalid staged file size"))?)))
            .collect()
    }
}

struct Transfer {
    script: String,
    // Each echoed command starts after all preceding commands succeeded.
    marks: Vec<(String, Progress)>,
}

fn transfer(
    cd: String,
    attempt: &Attempt,
    resume: bool,
    post: &Post,
    blobs: &[String],
    sizes: &HashMap<String, u64>,
) -> io::Result<Transfer> {
    let mut script = cd;
    let part = relative(&attempt.part)?;
    if !resume {
        script.push_str(&format!("mkdir {part}\n"));
    }
    script.push_str(&format!("cd {part}\n"));
    let total = post.media.iter().map(|m| m.bytes).sum();
    let mut bytes = 0u64;
    for m in &post.media {
        let size = sizes.get(&m.file).copied().unwrap_or(0);
        if size > m.bytes {
            return Err(invalid("a staged file exceeds the stored size"));
        }
        bytes += size;
    }
    let mut marks = Vec::new();
    for (m, blob) in post.media.iter().zip(blobs) {
        let path = relative(&m.file)?;
        let present = sizes.get(&m.file).copied();
        if present == Some(m.bytes) {
            continue;
        }
        if present.is_none() {
            // reput requires an existing destination, even on the first attempt.
            script.push_str(&format!("put \"empty\" {path}\n"));
        }
        let cmd = format!("reput {} {path}", quote(blob)?);
        marks.push((format!("sftp> {cmd}"), Progress { file: m.file.clone(), bytes, total }));
        script.push_str(&cmd);
        script.push('\n');
        bytes += m.bytes - present.unwrap_or(0);
    }
    let manifest = "put \"post.json\" \"./post.json\"";
    marks.push((format!("sftp> {manifest}"), Progress { file: "post.json".into(), bytes: total, total }));
    script.push_str(&format!(
        "{manifest}\ncd \"..\"\nrename {part} {}\nls {}\n",
        relative(&attempt.folder)?,
        relative(&format!("{}/post.json", attempt.folder))?
    ));
    Ok(Transfer { script, marks })
}

impl Upload {
    /// Resolve only local store IDs. Prepare on a worker; this reads the private index.
    pub fn prepare(store: Store, target: Target, ids: Vec<String>, title: String, text: String) -> io::Result<Self> {
        target.validate()?;
        if ids.is_empty() {
            return Err(invalid("select at least one media entry"));
        }
        let mut media = Vec::new();
        let mut used = HashSet::new();
        for id in &ids {
            let (mut item, _) = store.resolve(id)?;
            let ext = Path::new(&item.file).extension().and_then(|v| v.to_str()).unwrap();
            let stem = item.file.strip_suffix(ext).unwrap().trim_end_matches('.');
            let mut name = item.file.clone();
            if name.chars().any(char::is_control) {
                name = format!("{}.{}", item.sha256, ext);
            }
            let mut n = 2;
            while !used.insert(name.to_lowercase()) {
                let stem = if stem.chars().any(char::is_control) { &item.sha256 } else { stem };
                name = format!("{stem}-{n}.{ext}");
                n += 1;
            }
            item.file = name;
            media.push(item);
        }
        let post = Post { title, text, date: utc(SystemTime::now())?, media };
        Ok(Self { store, target, post, ids, token: nonce()?, attempt: Mutex::new(Attempt::default()) })
    }

    /// Submit only after local confirmation. The UI can poll the handle and the progress channel.
    pub fn start(
        self: &Arc<Self>,
        pool: &TaskPool,
        program: PathBuf,
        progress: Sender<Progress>,
    ) -> Result<TaskHandle<io::Result<Published>>, SubmitError> {
        let job = Arc::clone(self);
        pool.submit_named(Class::Background, "publish media", move || job.run(&program, &progress))
    }

    pub(super) fn run(&self, program: &Path, progress: &Sender<Progress>) -> io::Result<Published> {
        let mut attempt = self.attempt.try_lock().map_err(|_| io::Error::other("publish attempt already active"))?;
        if !attempt.verified {
            let scratch = Scratch::new(&self.store.home.join("media"))?;
            let batch = Batch { program, target: &self.target, scratch: &scratch.0 };
            let names = batch.names(None)?;
            let base = self.post.base();
            let manifest = self.post.manifest();
            let resume = !attempt.part.is_empty() && names.contains(&attempt.part);
            if !resume && !attempt.folder.is_empty() && names.contains(&attempt.folder) {
                // The connection may have been lost between rename and its acknowledgement.
                let path = relative(&format!("{}/post.json", attempt.folder))?;
                batch.run(&format!("{}get {path} \"received\"\n", batch.cd()?), |_| {})?;
                if fs::read(scratch.0.join("received"))? != manifest {
                    return Err(invalid("staging folder is missing and the destination contains a different post"));
                }
                attempt.verified = true;
            } else {
                attempt.folder = available_folder(&base, &names);
                if !resume {
                    attempt.part = part_folder(&attempt.folder, &self.token);
                }
                let sizes = if resume { batch.sizes(&attempt.part, &self.post)? } else { HashMap::new() };
                let blobs = self
                    .ids
                    .iter()
                    .zip(&self.post.media)
                    .map(|(id, selected)| {
                        let (media, path) = self.store.resolve(id)?;
                        if media.sha256 != selected.sha256 || media.bytes != selected.bytes {
                            return Err(invalid("selected store entry changed; review the post again"));
                        }
                        let file = path.file_name().and_then(|v| v.to_str()).ok_or_else(|| invalid("invalid blob name"))?;
                        Ok(format!("../{file}"))
                    })
                    .collect::<io::Result<Vec<_>>>()?;
                fs::write(scratch.0.join("empty"), [])?;
                fs::write(scratch.0.join("post.json"), manifest)?;
                let upload = transfer(batch.cd()?, &attempt, resume, &self.post, &blobs, &sizes)?;
                let mut marks = upload.marks.into_iter().peekable();
                batch
                    .run(&upload.script, |line| {
                        if marks.peek().is_some_and(|(cmd, _)| cmd == line) {
                            if let Some((_, event)) = marks.next() {
                                let _ = progress.send(event);
                            }
                        }
                    })
                    .map_err(|e| io::Error::other(format!("{e}. Publish not verified. Retry staging folder {}", attempt.part)))?;
                attempt.verified = true;
            }
        }
        self.record(&attempt.folder)?;
        let url = self.target.url_base.as_ref().map(|base| format!("{}/{}/", base.trim_end_matches('/'), attempt.folder));
        Ok(Published { folder: attempt.folder.clone(), url })
    }

    fn record(&self, folder: &str) -> io::Result<()> {
        // The home has one owning process. Serialize concurrent posts' read-modify-write.
        static HISTORY: Mutex<()> = Mutex::new(());
        let _lock = HISTORY.lock().map_err(|_| io::Error::other("publish history lock failed"))?;
        let path = self.store.home.join("publish-history.json");
        let mut rows = match fs::read(&path) {
            Ok(bytes) => match parse(&bytes).map_err(invalid)? {
                Value::Arr(rows) => rows,
                _ => return Err(invalid("invalid publish history")),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        if !rows.iter().any(|r| field(r, "id").ok() == Some(self.token.as_str())) {
            rows.push(obj(vec![
                ("id", s(&self.token)),
                ("folder", s(folder)),
                ("title", s(&self.post.title)),
                ("target", s(&self.target.name)),
                ("hashes", Value::Arr(self.post.media.iter().map(|m| s(&m.sha256)).collect())),
                ("time", s(utc(SystemTime::now())?)),
            ]));
            write_private(&path, Value::Arr(rows).to_json().as_bytes())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::post::{Kind, Media};

    fn target(dir: &str) -> Target {
        Target { name: "Demo".into(), ssh: "example.invalid".into(), dir: dir.into(), url_base: None }
    }

    #[test]
    fn literal_batch_arguments_and_final_check() {
        let target = target("drop box");
        assert_eq!(argv(&target).unwrap(), ["-b", "-", "-o", "BatchMode=yes", "example.invalid"]);
        let batch = Batch { program: Path::new("sftp"), target: &target, scratch: Path::new(".") };
        let media = Media {
            file: r#"-! '$;[x]*?\frame".png"#.into(),
            kind: Kind::Image,
            bytes: 7,
            sha256: "00".repeat(32),
            width: None,
            height: None,
            duration_s: None,
        };
        let post = Post { title: "; !\n../ Demo".into(), text: "".into(), date: "2000-01-01T00:00:00Z".into(), media: vec![media] };
        let folder = post.base();
        let attempt = Attempt { part: part_folder(&folder, "ab"), folder, verified: false };
        let transfer = transfer(batch.cd().unwrap(), &attempt, false, &post, &["../blob.png".into()], &HashMap::new()).unwrap();
        assert_eq!(
            transfer.script,
            r#"@cd "drop box"
mkdir "./.2000-01-01-demo.part-ab"
cd "./.2000-01-01-demo.part-ab"
put "empty" "./-! '$;[x]*?\\frame\".png"
reput "../blob.png" "./-! '$;[x]*?\\frame\".png"
put "post.json" "./post.json"
cd ".."
rename "./.2000-01-01-demo.part-ab" "./2000-01-01-demo"
ls "./2000-01-01-demo/post.json"
"#
        );
        assert!(quote("a\n!b").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn failed_rename_resumes_and_success_requires_the_final_manifest() {
        use crate::publish::store::{test_image, Author};
        use std::os::unix::fs::PermissionsExt;
        let home = Scratch::new(&std::env::temp_dir()).unwrap();
        let store = Store::new(&home.0);
        let image = test_image(&home.0);
        let id = store.import(&image, Author::Local).unwrap().id;
        let mut destination = target(".");
        destination.url_base = Some("https://example.invalid/posts/".into());
        let job = Upload::prepare(store, destination, vec![id], "Demo".into(), "A post".into()).unwrap();
        let base = job.post.base();
        let part = part_folder(&base, &job.token);
        let server = home.0.join("remote");
        fs::create_dir(&server).unwrap();
        let program = home.0.join("fake");
        fs::write(
            &program,
            r##"#!/bin/sh
set -eu
[ "$#" = 5 ] && [ "$1" = -b ] && [ "$2" = - ] && [ "$3" = -o ] && [ "$4" = BatchMode=yes ] && [ "$5" = example.invalid ] || exit 8
root="$PWD/../../remote"
remote="$root"
while IFS= read -r line; do
    printf '%s\n' "$line" >> "$root/trace"
    case "$line" in @*) ;; *) printf 'sftp> %s\n' "$line";; esac
    args=${line#* }
    args=${args#\"}
    args=${args%\"}
    sep='" "'
    first=${args%%"$sep"*}
    last=${args#*"$sep"}
    case "$line" in
        '@cd '*|'cd '*) remote=$(cd "$remote/$args" && pwd -P) ;;
        '@ls -1a '*) for f in "$remote"/* "$remote"/.[!.]*; do [ ! -e "$f" ] || printf './%s\n' "${f##*/}"; done ;;
        '@ls -ln '*) f=${line#*\"}; f=${f%\"}; printf '%s %s %s\n' '-rw------- 1 0 0' "$(wc -c < "$remote/$f")" "$f" ;;
        'mkdir '*) mkdir "$remote/$args" ;;
        'put '*) cp "$first" "$remote/$last" ;;
        'reput '*)
            size=$(wc -c < "$remote/$last")
            [ "$size" -lt "$(wc -c < "$first")" ]
            if [ -f "$root/interrupt" ]; then
                rm "$root/interrupt"
                printf GIF >> "$remote/$last"
                exit 2
            fi
            dd if="$first" bs=1 skip="$size" 2>/dev/null >> "$remote/$last" ;;
        'rename '*)
            if [ -f "$root/race" ]; then
                rm "$root/race"
                mkdir "$remote/$last"
                printf existing > "$remote/$last/post.json"
            fi
            if [ -e "$remote/$last" ]; then printf 'destination exists\n' >&2; exit 3; fi
            mv "$remote/$first" "$remote/$last"
            mv "$remote/$last/post.json" "$remote/$last/withheld" ;;
        'ls '*) [ -f "$remote/$args" ] || { printf 'missing final manifest\n' >&2; exit 4; } ;;
        'get '*) cp "$remote/$first" "$last" ;;
        *) exit 9 ;;
    esac
done
"##,
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(server.join("interrupt"), []).unwrap();
        fs::write(server.join("race"), []).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let history = home.0.join("publish-history.json");
        assert!(job.run(&program, &tx).is_err());
        assert_eq!(fs::read(server.join(&part).join("tiny.gif")).unwrap(), b"GIF");
        assert!(part.starts_with('.') && !server.join(&base).exists(), "an interrupted upload leaves no visible post");
        assert!(!server.join(&part).join("post.json").exists(), "post.json is written only after every media file");
        let error = job.run(&program, &tx).unwrap_err().to_string();
        assert!(error.contains("destination exists") && error.contains(&part), "{error}");
        assert_eq!(fs::read(server.join(&part).join("tiny.gif")).unwrap(), fs::read(&image).unwrap());
        assert!(server.join(&part).join("post.json").is_file());
        assert_eq!(fs::read(server.join(&base).join("post.json")).unwrap(), b"existing");
        let error = job.run(&program, &tx).unwrap_err().to_string();
        assert!(error.contains("missing final manifest"), "{error}");
        assert!(!history.exists());
        let folder = format!("{base}-2");
        let final_dir = server.join(&folder);
        fs::rename(final_dir.join("withheld"), final_dir.join("post.json")).unwrap();
        let published = job.run(&program, &tx).unwrap();
        assert_eq!(published.folder, folder);
        assert_eq!(published.url.unwrap(), format!("https://example.invalid/posts/{folder}/"));
        assert_eq!(job.run(&program, &tx).unwrap().folder, folder);
        assert!(!server.join(&part).exists());
        assert_eq!(fs::read(final_dir.join("post.json")).unwrap(), job.post.manifest());
        let trace = fs::read_to_string(server.join("trace")).unwrap();
        assert_eq!(trace.lines().filter(|s| s.starts_with("reput ")).count(), 2);
        assert_eq!(trace.lines().filter(|s| s.starts_with("put \"empty\"")).count(), 1);
        assert!(rx.try_iter().any(|p| p.file == "tiny.gif" && p.bytes == 3));
        let data = parse(&fs::read(history).unwrap()).unwrap();
        let rows = data.as_arr().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(field(&rows[0], "folder").unwrap(), folder);
    }
}
