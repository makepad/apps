use super::*;
use crate::{marks::{self, Head}, store_worker::StoredFile};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IssueProject { pub town: TownId, pub project: Id }

pub enum IssueEdit { Note(String), Resolved(bool), Delete }

impl Town {
    /// Chain each chunk to the preceding signature, including the new issue when filing a draft.
    pub(super) fn sign_mark(&self, key: &DeviceKey, target: Id, project: Option<Id>, width: i32, head: Head,
        pts: &[[i32; 2]], origin: Origin, events: &mut Vec<Event>) -> Result<(), String> {
        if width <= 0 || pts.len() < 2 || pts.iter().flatten().any(|n| !(0..=marks::UNIT).contains(n)) {
            return Err("A mark needs a positive width and at least two points inside the picture.".into())
        }
        let chunks = marks::chunks(pts);
        for (i, chunk) in chunks.iter().enumerate() {
            let arrow = head == Head::End && i + 1 == chunks.len();
            let fields = vec![(field::PARENT, FieldValue::Id(target)),
                (Key::Name("pts".into()), FieldValue::Literal(marks::encode_pts(chunk))),
                (Key::Name("width".into()), FieldValue::Literal(width.to_string())),
                (Key::Name("head".into()), FieldValue::Literal(if arrow { "1" } else { "0" }.into()))];
            let parents = events.last().map_or_else(|| self.log.parents_for(&key.public()), |e| vec![e.id]);
            events.push(self.sign(key, Content::Entity { kind: Key::Number(kind::STROKE), project, derive: None,
                fields, copied_from: vec![] }, origin, parents)?);
        }
        Ok(())
    }

    pub(super) fn save_issue_events(&mut self, events: Vec<Event>) -> Result<Id, String> {
        let first = events.first().ok_or("No issue or marks to save.")?.id;
        let from = self.log.events().len();
        self.log.append_all(events).map_err(|e| format!("Saving the issue marks: {e}"))?;
        self.fold_from(from);
        self.index_from(from);
        Ok(first)
    }
}

impl Model {
    pub(crate) fn issue_fold_stamp(&self) -> (usize, u64) {
        (self.open_event_count(), self.membership_epoch)
    }

    /// A fork or the arrival of a missing parent can refold older records. Otherwise only
    /// a change to this issue or to a stroke entering, on, or leaving it needs a new decode.
    pub(crate) fn issue_marks_touched(&self, since: (usize, u64), target: Id, was_on: impl Fn(&Id) -> bool) -> bool {
        if since.1 != self.membership_epoch { return true }
        let Some(events) = self.log.events().get(since.0..) else { return true };
        let related = |id: &Id| *id == target || self.state.entities.get(id).is_some_and(|e|
            e.kind == kind::STROKE && (parent(e) == Some(target) || was_on(id)));
        events.iter().any(|event| {
            if event.header.kind == makepad_town_core::event::kind::ENTITY { return related(&event.id) }
            crate::board::touched_by(event, |id| self.event(id)).iter().any(&related)
        })
    }

    pub(crate) fn store_issue(&mut self, draft: u64, town: TownId, still: crate::app_link::IssueStill) -> Result<(), String> {
        let worker = self.files.worker.as_mut().filter(|w| !w.stopped()).ok_or("Picture storage is unavailable.")?;
        worker.add_issue(draft, town, still.png, still.poster);
        worker.dispatch();
        self.files.pending_adds += 1;
        Ok(())
    }

    pub(crate) fn store_issue_video(&mut self, draft: u64, town: TownId, recording: crate::app_link::report::Recording) -> Result<(), String> {
        let worker = self.files.worker.as_mut().filter(|w| !w.stopped()).ok_or("Recording storage is unavailable.")?;
        worker.add_issue_video(draft, town, recording);
        worker.dispatch();
        self.files.pending_adds += 1;
        Ok(())
    }

    pub(crate) fn file_issue(&mut self, project: IssueProject, pictures: [StoredFile; 2], [w, h]: [u32; 2],
        label: &str, note: &str, session: Option<SessionId>, video: Option<(StoredFile, i64)>, source_peer: Option<PublicKey>,
        marks: &[(i32, Head, Vec<[i32; 2]>)]) -> Result<Id, String> {
        if !self.complete_blobs(&project.town).is_some_and(|held| pictures.iter().chain(video.as_ref().map(|(f, _)| f)).all(|f| held.contains(&f.id))) {
            return Err("Issue media is missing from storage.".into());
        }
        self.in_town(project.town, |r, key| {
            if !r.state.active_member(&key.public()) { return Err("This town is read-only now.".into()); }
            if !r.state.entities.get(&project.project).is_some_and(|e| e.kind == kind::PROJECT && live(e)) {
                return Err("The destination project is no longer active.".into());
            }
            let [picture, poster] = pictures.map(|f| FieldValue::Blob { size: f.size, root: f.root });
            let mut fields = vec![(field::BLOB, picture), (field::POSTER, poster),
                (field::W, FieldValue::Int(w.into())), (field::H, FieldValue::Int(h.into())),
                (field::LABEL, FieldValue::Text(label.into())), (field::TEXT, FieldValue::Text(note.into()))];
            if let Some((file, duration)) = video {
                fields.extend([(field::VIDEO, FieldValue::Blob { size: file.size, root: file.root }),
                    (field::DURATION, FieldValue::Int(duration)), (field::MEDIA, FieldValue::Text("video/mp4".into()))]);
            }
            if let Some(session) = session { fields.push((field::SESSION, FieldValue::Key(session.to_vec()))); }
            let open = session.and_then(|s| r.state.session(&s)?.open.as_ref());
            let peer = source_peer.or_else(|| open.map(|o| o.opener).filter(|p| *p != key.public()));
            let mut sources = Vec::new();
            if let Some(peer) = peer { sources.push(CopySource::SharePeer { peer }); }
            if let Some(open) = open { sources.push(CopySource::Record(open.anchor)); }
            let copied_from = entity_copy_refs(&fields, &[field::LABEL, field::BLOB, field::POSTER, field::VIDEO], &sources);
            let project = Some(project.project);
            let issue = r.sign(key, Content::Entity { copied_from, kind: Key::Number(kind::ISSUE), project,
                derive: None, fields }, Origin::Ui, r.log.parents_for(&key.public()))?;
            let target = issue.id;
            let mut events = vec![issue];
            for (width, head, pts) in marks {
                r.sign_mark(key, target, project, *width, *head, pts, Origin::Ui, &mut events)?;
            }
            r.save_issue_events(events)
        })
    }

    pub(crate) fn change_issue(&mut self, project: IssueProject, issue: Id, edit: IssueEdit) -> Result<(), String> {
        let (field, value) = match edit {
            IssueEdit::Note(text) => (field::TEXT, FieldValue::Text(text)),
            IssueEdit::Resolved(resolved) => (field::STATUS, FieldValue::Text(if resolved { "resolved" } else { "open" }.into())),
            IssueEdit::Delete => (field::DELETED, FieldValue::Bool(true)),
        };
        self.in_town(project.town, |r, key| {
            if !r.state.active_member(&key.public()) { return Err("This town is read-only now.".into()); }
            let e = r.state.entities.get(&issue).filter(|e| e.kind == kind::ISSUE && e.project == Some(project.project) && !e.flag(field::DELETED))
                .ok_or("This issue is no longer available.")?;
            if field != field::STATUS && r.state.seat_of(&key.public()) != r.state.seat_of(&e.author) {
                return Err("Only the reporter can change this issue's note or delete it.".into());
            }
            if e.value(field.clone()) != Some(&value) {
                r.append(key, Content::Set { copied_from: vec![], changes: vec![Change { entity: issue, field, value }], label: None }, Origin::Ui)?;
            }
            Ok(())
        })
    }

    pub(crate) fn project_issues(&self, project: Id) -> Vec<&Entity> {
        let mut issues: Vec<_> = self.state.entities.values().filter(|e|
            e.kind == kind::ISSUE && e.project == Some(project) && !e.flag(field::DELETED)).collect();
        issues.sort_unstable_by_key(|e| (std::cmp::Reverse(e.created), e.id));
        issues
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::{join, exchange, ShareHomes};

    #[test]
    fn visual_issue_sync_and_reload() {
        let homes = ShareHomes::new("visual-issue");
        let open = |name: &str| Model::open(&homes.0.join(name)).unwrap().0;
        let (mut ana, mut ben) = (open("Ana"), open("Ben"));
        ana.set_person_name("Ana").unwrap();
        ben.set_person_name("Ben").unwrap();
        ana.pin_linked(&ben.me(), 3, "Ben").unwrap();
        ben.pin_linked(&ana.me(), 3, "Ana").unwrap();
        ana.sync_start(&ben.me());
        ben.sync_start(&ana.me());
        let town = join(&mut ana, &mut ben, false);
        let project = IssueProject { town, project: ana.current_project().unwrap().id };
        let label = "Sketch · Canvas · 320.5 × 1 pt · 2 dpi";
        let size = [641, 2];
        let frame = makepad_ai_services::pictures::RgbaPicture { width: size[0], height: size[1], rgba: [4, 8, 12, 255].repeat(641 * 2) };
        let png = makepad_ai_services::pictures::encode_png(&frame).unwrap();
        let still = crate::app_link::IssueStill::from_png(png.clone(), size).unwrap();
        assert_eq!(*still.png, png, "the full PNG is exactly the app's frame");
        let small = makepad_ai_services::pictures::decode_png(&still.poster).unwrap();
        assert_eq!([small.width, small.height], [640, 2], "the poster fits the 640 pixel edge");
        assert_eq!(&small.rgba[..4], &frame.rgba[..4], "the poster comes from the same frame");
        let expected = [&still.png, &still.poster].map(|bytes| {
            let root = blob::Tree::new(bytes, blob::CHUNK).unwrap().root();
            blob::id(bytes.len() as u64, &root)
        });
        let (signal, wake) = std::sync::mpsc::channel();
        ana.start_store(crate::store_worker::test_pool(), move || { let _ = signal.send(()); }).unwrap();
        ana.store_issue(1, town, still).unwrap();
        let pictures = loop {
            if let Some((draft, result)) = ana.drain_store().issues.pop() {
                assert_eq!(draft, 1, "the receipt names the draft whose picture was stored");
                let crate::store_worker::IssueMedia::Picture(files) = result.unwrap() else { panic!("expected the picture receipt") };
                break files;
            }
            wake.recv().expect("the issue store wakes the model");
        };
        assert_eq!(pictures.map(|f| f.id), expected, "the store receipt identifies the original PNG and its poster");
        let before = ana.open_event_count();
        assert!(ana.project_issues(project.project).is_empty(), "storing pictures alone publishes no issue");
        let issue = ana.file_issue(project, pictures, size, label, "Clipped title", None, None, None, &[]).unwrap();
        assert_eq!(ana.open_event_count(), before + 1, "filing publishes both pictures and the note in one record");
        exchange(&mut ana, &mut ben);
        let seen = ben.project_issues(project.project);
        assert_eq!(seen.len(), 1, "Ben sees one issue in its project");
        let check_picture = |e: &Entity| {
            for (field, file) in [field::BLOB, field::POSTER].into_iter().zip(pictures) {
                assert_eq!(e.value(field.clone()), Some(&FieldValue::Blob { size: file.size, root: file.root }), "signed picture reference {field}");
            }
            assert_eq!((e.value(field::W), e.value(field::H)), (Some(&FieldValue::Int(641)), Some(&FieldValue::Int(2))), "native pixel size");
            assert_eq!(e.text(field::LABEL), label, "the captured window label travels with the issue");
        };
        check_picture(seen[0]);
        assert_eq!(ben.device_label(&seen[0].author), "Ana", "Ben sees the reporter's name");
        for file in pictures {
            assert_eq!(ben.references.descriptor(&file.id), Some((file.size, file.root)), "Ben can fetch each signed picture");
        }
        assert_eq!(seen[0].text(field::TEXT), "Clipped title", "the creation includes the draft note");
        ana.change_issue(project, issue, IssueEdit::Note("Clipped on resize".into())).unwrap();
        exchange(&mut ana, &mut ben);
        assert_eq!(ben.state.entities[&issue].text(field::TEXT), "Clipped on resize", "Ana's note edit reaches Ben");
        // Deliver a signed SET directly so this exercises the fold on both computers.
        ben.append(Content::Set { copied_from: vec![], label: None, changes: vec![
            Change { entity: issue, field: field::TEXT, value: FieldValue::Text("Other".into()) },
            Change { entity: issue, field: field::DELETED, value: FieldValue::Bool(true) },
        ] }, Origin::Ui).unwrap();
        ben.change_issue(project, issue, IssueEdit::Resolved(true)).unwrap();
        exchange(&mut ana, &mut ben);
        for m in [&ana, &ben] {
            let e = m.project_issues(project.project)[0];
            assert_eq!(e.text(field::TEXT), "Clipped on resize", "both folds ignore Ben's note and deletion");
            assert_eq!(e.text(field::STATUS), "resolved", "Ben can resolve Ana's issue on both copies");
        }
        ana.change_issue(project, issue, IssueEdit::Delete).unwrap();
        exchange(&mut ana, &mut ben);
        for m in [&ana, &ben] { assert!(m.project_issues(project.project).is_empty(), "Ana's deletion hides the issue on both copies"); }
        ana.stop_store();
        while !ana.store_stopped() {
            ana.drain_store();
            if !ana.store_stopped() { wake.recv().unwrap(); }
        }
        drop((ana, ben));
        for name in ["Ana", "Ben"] {
            let mut m = open(name);
            m.open_town(town).unwrap();
            assert!(m.project_issues(project.project).is_empty(), "deletion survives reload on {name}");
            let e = &m.state.entities[&issue];
            check_picture(e);
            assert_eq!(e.text(field::TEXT), "Clipped on resize", "the reporter's edit survives reload on {name}");
            assert_eq!(e.text(field::STATUS), "resolved", "the resolution survives reload on {name}");
        }
    }
}
