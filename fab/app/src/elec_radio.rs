//! The Firmware pane's ESP32-S3 radio pack. Only Get starts a download.

use fab_elec_fw::radio::{link_installed, pack_dest, pack_staging, packs_dir};
use fab_elec_mcu_island::Island;
use fab_elec_mcu_pack::{espressif_s3, fetch, install, verify, CancelToken, Manifest, Progress};
use fab_elec_view::live::sim::{RadioLive, RadioState};
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle, ToUIReceiver, ToUISender};
use makepad_widgets::{Cx, Event};
use std::path::{Path, PathBuf};
use std::sync::Arc;

enum Msg {
    Progress(Progress),
    Installing,
    Linking,
    /// A local check can finish with no verified installation.
    Done(Result<Option<Arc<Island>>, String>),
}

pub(crate) struct RadioPack {
    manifest: Arc<Manifest>,
    packs: PathBuf,
    cancel: CancelToken,
    rx: ToUIReceiver<Msg>,
    job: Option<TaskHandle<()>>,
    live: RadioLive,
    island: Option<Arc<Island>>,
}

impl RadioPack {
    pub(crate) fn new(cx: &mut Cx) -> Self {
        let manifest = Arc::new(espressif_s3());
        let mut radio = Self {
            live: RadioLive {
                state: RadioState::Linking,
                total_bytes: manifest.total_bytes(),
            },
            manifest: manifest.clone(),
            packs: packs_dir(),
            cancel: CancelToken::new(),
            rx: ToUIReceiver::default(),
            job: None,
            island: None,
        };
        let dest = pack_dest(&radio.packs);
        radio.start(cx, "check installed radio pack", move |_| {
            // link_installed verifies first. On failure, distinguish a missing or
            // damaged pack (Get is available) from a linker error in a verified pack.
            match link_installed(&dest) {
                Ok(island) => Ok(Some(Arc::new(island))),
                Err(_) if verify(&manifest, &dest).is_err() => Ok(None),
                Err(error) => Err(error),
            }
        });
        radio
    }

    pub(crate) fn live(&self) -> &RadioLive {
        &self.live
    }

    pub(crate) fn island(&self) -> Option<&Arc<Island>> {
        self.island.as_ref()
    }

    fn start(
        &mut self,
        cx: &mut Cx,
        name: &'static str,
        work: impl FnOnce(&ToUISender<Msg>) -> Result<Option<Arc<Island>>, String> + Send + 'static,
    ) {
        let tx = self.rx.sender();
        match cx
            .task_pool()
            .submit_named(Class::Background, name, move || {
                let result = work(&tx);
                let _ = tx.send(Msg::Done(result));
            }) {
            Ok(job) => self.job = Some(job),
            Err(error) => {
                self.live.state =
                    RadioState::Failed(format!("the radio job did not start: {error}"))
            }
        }
    }

    pub(crate) fn get(&mut self, cx: &mut Cx) {
        if self.job.is_some()
            || !matches!(
                self.live.state,
                RadioState::Absent | RadioState::Failed(_) | RadioState::Cancelled
            )
        {
            return;
        }
        self.cancel = CancelToken::new();
        let cancel = self.cancel.clone();
        let manifest = self.manifest.clone();
        let packs = self.packs.clone();
        self.live.state = RadioState::Fetching {
            done: 0,
            file: String::new(),
        };
        self.start(cx, "get radio pack", move |tx| {
            download_and_link(&manifest, &packs, &cancel, tx).map(|island| Some(Arc::new(island)))
        });
    }

    pub(crate) fn cancel(&self) {
        if matches!(self.live.state, RadioState::Fetching { .. }) {
            self.cancel.cancel();
        }
    }

    /// Signals wake the UI. Apply the newest message once on the following frame,
    /// so a burst of writes cannot repaint the pane for every chunk.
    pub(crate) fn event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let Some(job) = &mut self.job else {
            return false;
        };
        match event {
            Event::Signal => {
                cx.new_next_frame();
                return false;
            }
            Event::NextFrame(_) => {}
            _ => return false,
        }
        let message = match self.rx.try_recv_flush() {
            Ok(message) => message,
            Err(_) => match job.try_take() {
                Some(Err(error)) => Msg::Done(Err(format!("the radio job stopped: {error}"))),
                _ => return false,
            },
        };
        let state = match message {
            Msg::Progress(progress) => {
                // The row already identifies the pack; show the path within the repository.
                let file = progress
                    .file
                    .split_once('/')
                    .map_or(progress.file.as_str(), |(_, path)| path);
                RadioState::Fetching {
                    done: progress.bytes_done,
                    file: file.into(),
                }
            }
            Msg::Installing => RadioState::Installing,
            Msg::Linking => RadioState::Linking,
            Msg::Done(result) => {
                self.job = None;
                match result {
                    _ if self.cancel.is_cancelled() => RadioState::Cancelled,
                    Ok(Some(island)) => RadioState::Ready {
                        island_bytes: self.island.insert(island).image.len() as u64,
                    },
                    Ok(None) => RadioState::Absent,
                    Err(reason) => RadioState::Failed(reason),
                }
            }
        };
        if self.live.state == state {
            return false;
        }
        self.live.state = state;
        true
    }
}

impl Drop for RadioPack {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn download_and_link(
    manifest: &Manifest,
    packs: &Path,
    cancel: &CancelToken,
    tx: &ToUISender<Msg>,
) -> Result<Island, String> {
    let staging = pack_staging(packs);
    let dest = pack_dest(packs);
    fetch(manifest, None, &staging, cancel, |progress| {
        let _ = tx.send(Msg::Progress(progress));
    })
    .map_err(|error| error.to_string())?;
    if cancel.is_cancelled() {
        return Err("cancelled".into());
    }
    let _ = tx.send(Msg::Installing);
    let dest = install(manifest, &staging, &dest).map_err(|error| error.to_string())?;
    if cancel.is_cancelled() {
        return Err("cancelled".into());
    }
    let _ = tx.send(Msg::Linking);
    link_installed(&dest)
}
