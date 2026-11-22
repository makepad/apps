//! Fab Arms' lab: the cells, parts and jobs the Arm workspace runs in the virtual cell, the
//! session that steps a job through the planner and the cell, the printer every arm is timed
//! against, and the checks (the dynamics', the loop's end to end) that the tests assert and the
//! workspace lists.

pub mod checks;
pub mod compare;
pub mod fdm;
pub mod lab;
pub mod printer;
pub mod realtime;

#[cfg(test)]
mod tests;

