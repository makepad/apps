//! A verified copy of a journal prefix, or an I/O error: the replay core in State mode.
use super::{replay::{state, Outcome}, *};

pub fn rebuild(journal_dir: &Path, upto: Option<u64>, into: &Path) -> io::Result<()> {
    let report = state(journal_dir, upto, into, &mut |_| {})?;
    match report.stopped.map(|step| step.outcome) {
        Some(Outcome::Gap { reason }) => Err(io::Error::new(io::ErrorKind::InvalidData, reason)),
        _ => Ok(()),
    }
}
