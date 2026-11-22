//! A test print's edits, in the plate's frame: what the list, the fields, the gizmo and
//! `fab.test_print` all ask for, turned here into the changes the project keeps, in its part's
//! frame, so each reaches the same program by one path.

use crate::eval::Evaluated;
use crate::project::{self, PlacedBox, Project, TestEdit, TestPrint};
use crate::{program, Change};

impl Evaluated {
    /// Where a test print stands on this plate; none when its part is not on it.
    pub fn test_print_box(&self, t: &TestPrint) -> Option<PlacedBox> {
        Some(t.placed(self.test_print_frame(t)?))
    }

    /// How a test print's piece lies on this plate, as the turn from as printed.
    pub fn test_print_lay(&self, t: &TestPrint) -> Option<[[f64; 3]; 3]> {
        t.laid(self.test_print_frame(t)?)
    }

    /// The frame a test print lives in: its part's.
    fn test_print_frame(&self, t: &TestPrint) -> Option<&[[f64; 4]; 3]> {
        let id = t.part();
        let i = self.plate.placed_parts().position(|p| p.object == id)?;
        self.placed_frame(i)
    }

    /// The project changes `edits` make, one undo step together. What is wrong says which test
    /// print, and what would put it right.
    pub fn test_print_changes(&self, project: &Project, edits: &[TestEdit]) -> Result<Vec<Change>, String> {
        let mut taken: Vec<String> = project.test_prints.iter().map(|t| t.name.clone()).collect();
        let mut out = Vec::new();
        for edit in edits {
            out.push(match edit {
                TestEdit::Add { object, center, size, turn, name, on, in_model } => {
                    let name = name.clone().unwrap_or_else(|| project::next_test_name(&taken));
                    if taken.contains(&name) {
                        return Err(format!("test print '{name}': the name is taken: give it another, or edit that one with set"));
                    }
                    let Some((id, solid)) = program::split_id(object) else { return Err(format!("test print '{name}': '{object}' is no part: name it as instance/object, as fab.inspect lists them")) };
                    let (instance, link) = program::split_link(id);
                    let mut t = TestPrint { name: name.clone(), instance: instance.to_string(), object: solid.to_string(), link, at: [0.0; 3], size: [1.0; 3], turn: [0.0; 3], pose: None, on: *on, brim: false };
                    let Some(frame) = self.test_print_frame(&t) else { return Err(format!("test print '{name}': no part '{object}' on the plate: fab.inspect lists its parts")) };
                    if *in_model {
                        (t.at, t.size, t.turn) = (*center, *size, *turn);
                    } else {
                        t.set_placed(PlacedBox { center: *center, size: *size, turn: *turn }, frame);
                    }
                    taken.push(name.clone());
                    Change::TestPrint { name, set: Some(t) }
                }
                TestEdit::Set { name, center, size, turn, pose, on, brim, to } => {
                    let Some(was) = project.test_print(name) else { return Err(format!("test print '{name}': there is none of that name: fab.inspect lists them")) };
                    let mut t = was.clone();
                    if center.is_some() || size.is_some() || turn.is_some() {
                        let Some(frame) = self.test_print_frame(&t) else { return Err(format!("test print '{name}': its part '{}' is not on the plate", t.part())) };
                        let mut placed = t.placed(frame);
                        placed.center = center.unwrap_or(placed.center);
                        placed.size = size.unwrap_or(placed.size);
                        placed.turn = turn.unwrap_or(placed.turn);
                        t.set_placed(placed, frame);
                    }
                    t.pose = pose.unwrap_or(t.pose);
                    t.on = on.unwrap_or(t.on);
                    t.brim = brim.unwrap_or(t.brim);
                    if let Some(to) = to {
                        t.name = to.clone();
                    }
                    Change::TestPrint { name: name.clone(), set: Some(t) }
                }
                TestEdit::Remove { name } => Change::TestPrint { name: name.clone(), set: None },
                TestEdit::LayFlat { name, normal } => {
                    let Some(was) = project.test_print(name) else { return Err(format!("test print '{name}': there is none of that name: fab.inspect lists them")) };
                    // Scale first so every finite nonzero direction can be made unit without overflow.
                    let scale = normal.iter().fold(0.0_f64, |m, x| m.max(x.abs()));
                    if normal.iter().any(|x| !x.is_finite()) || scale == 0.0 {
                        return Err(format!("test print '{name}': lay flat on {normal:?}: a direction is three finite numbers, not all zero"));
                    }
                    let mut t = was.clone();
                    let Some(frame) = self.test_print_frame(&t) else { return Err(format!("test print '{name}': its part '{}' is not on the plate", t.part())) };
                    t.lay_on(normal.map(|x| x / scale), frame);
                    Change::TestPrint { name: name.clone(), set: Some(t) }
                }
            });
        }
        Ok(out)
    }
}
