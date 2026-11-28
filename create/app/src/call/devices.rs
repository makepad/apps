//! The local call pickers. Only a person's pick produces text to persist.
//! A device event can change a route, the menu rows, or the unplug notice independently.

use makepad_widgets::makepad_platform::audio::{AudioDeviceDesc, AudioDeviceId, AudioDeviceType};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceKind {
    Input,
    Output,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceRow {
    pub id: AudioDeviceId,
    pub name: String,
    pub current: bool,
    pub is_default: bool,
    /// Still listed so the picker can explain an unavailable device.
    pub has_failed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceNotice {
    Unplugged { name: String, using: String },
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct PickerChange {
    /// None leaves the route alone. Some(None) closes it.
    pub chosen: Option<Option<AudioDeviceId>>,
    pub rows_changed: bool,
    /// None leaves the notice alone. Some(None) clears it.
    pub notice: Option<Option<DeviceNotice>>,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct DeviceChanges {
    pub input: PickerChange,
    pub output: PickerChange,
}

/// The entire contents for the next write of local/call-devices.
#[derive(Debug, Eq, PartialEq)]
pub struct Remember(pub String);

#[derive(Default)]
struct Picker {
    remembered: Option<String>,
    // A name survives restarts; an explicit ID distinguishes identical labels on this run.
    picked: Option<AudioDeviceId>,
    rows: Vec<DeviceRow>,
    current: Option<usize>,
    // Retain the unplug across an empty list until there is a fallback to name.
    missing: Option<String>,
    notice: Option<DeviceNotice>,
}

impl Picker {
    fn current(&self) -> Option<&DeviceRow> {
        self.current.map(|i| &self.rows[i])
    }

    fn update(&mut self, kind: DeviceKind, descs: &[AudioDeviceDesc]) -> PickerChange {
        // AudioDeviceType::is_input() also accepts loopback capture.
        let device_type = match kind {
            DeviceKind::Input => AudioDeviceType::Input,
            DeviceKind::Output => AudioDeviceType::Output,
        };
        let mut rows: Vec<_> = descs
            .iter()
            .filter(|d| d.device_type == device_type)
            .map(|d| DeviceRow {
                id: d.device_id,
                name: d.name.clone(),
                current: false,
                is_default: d.is_default,
                has_failed: d.has_failed,
            })
            .collect();
        let previous = self.current();
        let old_id = previous.map(|r| r.id);
        let healthy_id = |id: AudioDeviceId| rows.iter().position(|r| r.id == id && !r.has_failed);
        let still_here = old_id.and_then(healthy_id);
        let pinned = self.picked.and_then(healthy_id);
        let matches_name = |r: &DeviceRow| Some(r.name.as_str()) == self.remembered.as_deref();

        // Within a priority level preserve the current healthy ID across enumeration reorders.
        let chosen = pinned
            .or_else(|| still_here.filter(|&i| matches_name(&rows[i])))
            .or_else(|| rows.iter().position(|r| !r.has_failed && matches_name(r)))
            .or_else(|| still_here.filter(|&i| rows[i].is_default))
            .or_else(|| rows.iter().position(|r| !r.has_failed && r.is_default))
            .or_else(|| match kind {
                DeviceKind::Input => None,
                DeviceKind::Output => still_here.or_else(|| rows.iter().position(|r| !r.has_failed)),
            });

        let vanished = previous.filter(|old| !rows.iter().any(|r| r.id == old.id)).map(|old| old.name.clone());
        self.picked = pinned.map(|i| rows[i].id);
        if self.missing.as_ref().is_some_and(|name| rows.iter().any(|r| !r.has_failed && r.name == *name)) {
            self.missing = None;
        }
        if self.missing.is_none() {
            self.missing = vanished;
        }
        if let Some(i) = chosen {
            rows[i].current = true;
            if matches_name(&rows[i]) || self.missing.as_deref() == Some(rows[i].name.as_str()) {
                self.missing = None;
            }
        }
        let notice = self
            .missing
            .as_ref()
            .zip(chosen)
            .map(|(name, i)| DeviceNotice::Unplugged { name: name.clone(), using: rows[i].name.clone() });
        let new_id = chosen.map(|i| rows[i].id);
        let change = PickerChange {
            chosen: (old_id != new_id).then_some(new_id),
            rows_changed: rows != self.rows,
            notice: (self.notice != notice).then(|| notice.clone()),
        };
        self.rows = rows;
        self.current = chosen;
        self.notice = notice;
        change
    }

    fn pick(&mut self, id: AudioDeviceId) -> Option<PickerChange> {
        let next = self.rows.iter().position(|r| r.id == id && !r.has_failed)?;
        let switched = self.current != Some(next);
        if let Some(i) = self.current {
            self.rows[i].current = false;
        }
        self.rows[next].current = true;
        self.current = Some(next);
        self.remembered = Some(self.rows[next].name.clone());
        self.picked = Some(id);
        self.missing = None;
        Some(PickerChange {
            chosen: switched.then_some(Some(id)),
            rows_changed: switched,
            notice: self.notice.take().map(|_| None),
        })
    }
}

#[derive(Default)]
pub struct Devices {
    input: Picker,
    output: Picker,
}

impl Devices {
    pub fn new(remembered: &str) -> Self {
        let mut devices = Self::default();
        for line in remembered.lines() {
            let Some((kind, name)) = line.split_once(' ') else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            match kind {
                "input" => devices.input.remembered = Some(name.to_owned()),
                "output" => devices.output.remembered = Some(name.to_owned()),
                _ => {}
            }
        }
        devices
    }

    pub fn update(&mut self, descs: &[AudioDeviceDesc]) -> DeviceChanges {
        DeviceChanges {
            input: self.input.update(DeviceKind::Input, descs),
            output: self.output.update(DeviceKind::Output, descs),
        }
    }

    /// A stale menu action, wrong kind or failed device has no effect and writes nothing.
    pub fn pick(&mut self, kind: DeviceKind, id: AudioDeviceId) -> Option<(DeviceChanges, Remember)> {
        let mut change = DeviceChanges::default();
        match kind {
            DeviceKind::Input => change.input = self.input.pick(id)?,
            DeviceKind::Output => change.output = self.output.pick(id)?,
        }
        let mut text = String::new();
        for (prefix, name) in [("input ", &self.input.remembered), ("output ", &self.output.remembered)] {
            if let Some(name) = name {
                // A multiline or empty label can be picked by ID for this session. It cannot
                // be written as a name on one line without changing another preference.
                if name.is_empty() || name.contains(['\n', '\r']) {
                    continue;
                }
                text.push_str(prefix);
                text.push_str(name);
                text.push('\n');
            }
        }
        Some((change, Remember(text)))
    }

    fn picker(&self, kind: DeviceKind) -> &Picker {
        match kind {
            DeviceKind::Input => &self.input,
            DeviceKind::Output => &self.output,
        }
    }

    pub fn inputs(&self) -> &[DeviceRow] {
        &self.input.rows
    }
    pub fn outputs(&self) -> &[DeviceRow] {
        &self.output.rows
    }
    pub fn chosen(&self, kind: DeviceKind) -> Option<AudioDeviceId> {
        self.picker(kind).current().map(|r| r.id)
    }
    pub fn label(&self, kind: DeviceKind) -> &str {
        self.picker(kind).current().map(|r| r.name.as_str()).unwrap_or(match kind {
            DeviceKind::Input => "No microphone",
            DeviceKind::Output => "No speaker",
        })
    }
    pub fn notice(&self, kind: DeviceKind) -> Option<&DeviceNotice> {
        self.picker(kind).notice.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::LiveId;
    use AudioDeviceType::{Input, Loopback, Output};

    fn id(n: u64) -> AudioDeviceId {
        AudioDeviceId(LiveId(n))
    }

    fn desc(n: u64, ty: AudioDeviceType, name: &str, is_default: bool) -> AudioDeviceDesc {
        AudioDeviceDesc {
            device_id: id(n),
            device_type: ty,
            name: name.to_owned(),
            is_default,
            has_failed: false,
            channel_count: 2,
        }
    }

    fn unplugged(name: &str, using: &str) -> DeviceNotice {
        DeviceNotice::Unplugged { name: name.to_owned(), using: using.to_owned() }
    }

    #[test]
    fn remembered_devices_beat_defaults_and_missing_names_start_quietly() {
        let descs = [
            desc(1, Input, "Mic", true),
            desc(2, Input, "USB", false),
            desc(3, Output, "Speakers", true),
            desc(4, Output, "USB", false),
        ];
        for (text, mic, speaker) in [("input USB\noutput USB", 2, 4), ("input Missing\noutput Missing", 1, 3)] {
            let mut d = Devices::new(text);
            let change = d.update(&descs);
            assert_eq!((change.input.chosen, change.output.chosen), (Some(Some(id(mic))), Some(Some(id(speaker)))));
            assert_eq!((change.input.notice, change.output.notice), (None, None));
            assert_eq!(d.update(&descs), DeviceChanges::default());
            assert_eq!(
                d.inputs().iter().map(|r| (r.id, r.current, r.is_default)).collect::<Vec<_>>(),
                [(id(1), mic == 1, true), (id(2), mic == 2, false)]
            );
            assert_eq!(
                d.outputs().iter().map(|r| (r.id, r.current, r.is_default)).collect::<Vec<_>>(),
                [(id(3), speaker == 3, true), (id(4), speaker == 4, false)]
            );
        }
    }

    #[test]
    fn duplicate_name_pick_survives_an_unrelated_hotplug() {
        let mut d = Devices::new("input Twin");
        let mut list = vec![desc(1, Input, "Twin", false), desc(2, Input, "Twin", false)];
        assert_eq!(d.update(&list).input.chosen, Some(Some(id(1))));
        let (picked, Remember(text)) = d.pick(DeviceKind::Input, id(2)).unwrap();
        assert_eq!(picked.input.chosen, Some(Some(id(2))));
        assert_eq!(text, "input Twin\n");
        list.push(desc(3, Input, "New microphone", true));
        let added = d.update(&list);
        assert!(added.input.rows_changed);
        assert_eq!(added.input.chosen, None);
        assert!(d.inputs()[1].current);
        assert_eq!(d.inputs()[2].name, "New microphone");
        let mut next_session = Devices::new(&text);
        assert_eq!(next_session.update(&list).input.chosen, Some(Some(id(1))));

        let removed = list.remove(1);
        assert_eq!(d.update(&list).input.chosen, Some(Some(id(1))));
        list.insert(0, removed);
        assert_eq!(d.update(&list).input.chosen, None, "keep the healthy same-name replacement");
        assert_eq!(d.chosen(DeviceKind::Input), Some(id(1)));
    }

    #[test]
    fn unplug_and_replug_report_each_route_independently() {
        let mut d = Devices::new("input Headset mic\noutput Headset speakers");
        let mut list = vec![
            desc(1, Input, "Internal mic", true),
            desc(2, Output, "Internal speakers", true),
            desc(3, Input, "Headset mic", false),
            desc(4, Output, "Headset speakers", false),
        ];
        d.update(&list);
        list.truncate(2);
        let unplug = d.update(&list);
        assert_eq!(unplug.input.chosen, Some(Some(id(1))));
        assert_eq!(unplug.output.chosen, Some(Some(id(2))));
        let mic_notice = unplugged("Headset mic", "Internal mic");
        let speaker_notice = unplugged("Headset speakers", "Internal speakers");
        assert_eq!(unplug.input.notice, Some(Some(mic_notice.clone())));
        assert_eq!(unplug.output.notice, Some(Some(speaker_notice.clone())));
        assert_eq!(d.notice(DeviceKind::Input), Some(&mic_notice));
        assert_eq!(d.update(&list), DeviceChanges::default());

        let mut returning = desc(5, Input, "Headset mic", false);
        returning.has_failed = true;
        list.push(returning);
        let unavailable = d.update(&list);
        assert!(unavailable.input.rows_changed);
        assert_eq!((unavailable.input.chosen, unavailable.input.notice), (None, None));
        assert_eq!(d.notice(DeviceKind::Input), Some(&mic_notice));
        list[2].has_failed = false;
        let mic_back = d.update(&list);
        assert_eq!(mic_back.input.chosen, Some(Some(id(5))));
        assert_eq!(mic_back.input.notice, Some(None));
        assert_eq!(mic_back.output, PickerChange::default());
        assert_eq!(d.notice(DeviceKind::Output), Some(&speaker_notice));
        list.push(desc(6, Output, "Headset speakers", false));
        let speaker_back = d.update(&list);
        assert_eq!(speaker_back.output.chosen, Some(Some(id(6))));
        assert_eq!(speaker_back.output.notice, Some(None));
        assert_eq!(d.notice(DeviceKind::Output), None);
    }

    #[test]
    fn loopback_and_nondefault_microphones_never_substitute_for_a_default() {
        let mut d = Devices::new("input System audio");
        assert_eq!(d.label(DeviceKind::Input), "No microphone");
        let mut list = vec![
            desc(1, Loopback, "System audio", true),
            desc(2, Input, "Other mic", false),
            desc(3, Output, "Speakers", true),
        ];
        assert_eq!(d.update(&list).input.chosen, None);
        assert_eq!(d.inputs().len(), 1);
        assert_eq!(d.inputs()[0].id, id(2));
        assert!(d.pick(DeviceKind::Input, id(1)).is_none());
        assert!(d.pick(DeviceKind::Input, id(3)).is_none());

        list.push(desc(4, Input, "Default mic", true));
        assert_eq!(d.update(&list).input.chosen, Some(Some(id(4))));
        list[3].has_failed = true;
        let failed = d.update(&list);
        assert_eq!(failed.input.chosen, Some(None));
        assert_eq!(failed.input.notice, None, "failure is not an unplug");
        assert!(d.inputs()[1].has_failed && !d.inputs()[1].current);
        assert!(d.pick(DeviceKind::Input, id(4)).is_none());
        d.update(&[]);
        assert!(d.inputs().is_empty());
        assert_eq!(d.chosen(DeviceKind::Input), None);
        assert_eq!(d.label(DeviceKind::Input), "No microphone");
    }

    #[test]
    fn outputs_use_only_healthy_fallbacks() {
        let mut d = Devices::new("output Default");
        let mut list = vec![
            desc(1, Output, "Default", true),
            desc(2, Output, "Spare", false),
            desc(3, Output, "Another spare", false),
        ];
        list[0].has_failed = true;
        assert_eq!(d.update(&list).output.chosen, Some(Some(id(2))));
        list.swap(1, 2);
        assert_eq!(d.update(&list).output.chosen, None, "reordering equal fallbacks keeps the speaker");
        for device in &mut list {
            device.has_failed = true;
        }
        let failed = d.update(&list);
        assert_eq!(failed.output.chosen, Some(None));
        assert_eq!(failed.output.notice, None);
        assert_eq!(d.label(DeviceKind::Output), "No speaker");
        assert!(d.pick(DeviceKind::Output, id(1)).is_none());
        assert_eq!(d.update(&list), DeviceChanges::default());
        list[0].has_failed = false;
        assert_eq!(d.update(&list).output.chosen, Some(Some(id(1))));
    }

    #[test]
    fn pick_writes_both_preferences_and_round_trips() {
        let list = [desc(1, Input, "USB Mic", true), desc(2, Output, "Studio  L / R", true)];
        let mut d = Devices::new("unknown field\r\ninput Old mic\r\noutput Old speakers\r\n");
        d.update(&list);
        let (change, Remember(text)) = d.pick(DeviceKind::Input, id(1)).unwrap();
        assert_eq!(change, DeviceChanges::default(), "can remember the current fallback without reopening it");
        assert_eq!(text, "input USB Mic\noutput Old speakers\n");
        let (_, Remember(text)) = d.pick(DeviceKind::Output, id(2)).unwrap();
        assert_eq!(text, "input USB Mic\noutput Studio  L / R\n");
        let mut restored = Devices::new(&text);
        let mut with_new_defaults = list.to_vec();
        for device in &mut with_new_defaults {
            device.is_default = false;
        }
        with_new_defaults.extend([desc(3, Input, "New mic", true), desc(4, Output, "New speakers", true)]);
        let change = restored.update(&with_new_defaults);
        assert_eq!((change.input.chosen, change.output.chosen), (Some(Some(id(1))), Some(Some(id(2)))));
        assert_eq!(restored.label(DeviceKind::Output), "Studio  L / R");
    }

    #[test]
    fn unplug_with_no_replacement_and_a_pick_while_on_fallback() {
        let mut d = Devices::new("input USB");
        let usb = desc(1, Input, "USB", false);
        d.update(std::slice::from_ref(&usb));
        let gone = d.update(&[]);
        assert_eq!(gone.input.chosen, Some(None));
        assert_eq!(gone.input.notice, None);
        assert!(d.pick(DeviceKind::Input, id(1)).is_none());
        let internal = desc(2, Input, "Internal", true);
        let fallback = d.update(std::slice::from_ref(&internal));
        assert_eq!(fallback.input.notice, Some(Some(unplugged("USB", "Internal"))));
        let (picked, Remember(text)) = d.pick(DeviceKind::Input, id(2)).unwrap();
        assert_eq!(picked.input.chosen, None);
        assert_eq!(picked.input.notice, Some(None));
        assert_eq!(text, "input Internal\n");
        assert_eq!(d.update(&[internal, usb]).input.chosen, None);
    }

    #[test]
    fn multiline_label_cannot_overwrite_the_other_preference() {
        let mut d = Devices::new("output My speakers");
        d.update(&[desc(1, Input, "Mic\noutput Other speakers", false)]);
        let (_, Remember(text)) = d.pick(DeviceKind::Input, id(1)).unwrap();
        assert_eq!(d.chosen(DeviceKind::Input), Some(id(1)));
        assert_eq!(text, "output My speakers\n");
    }
}
