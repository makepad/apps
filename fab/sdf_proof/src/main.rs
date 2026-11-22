//! `fab-sdf-proof --prove-gyroid`: proves the gyroid's declared bounds again over its period cube
//! (half a minute of every core) and records the proof's fingerprint, which the suite checks
//! (`gyroid_proof`). The CI runs it on every pass (ci.toml `runs`). Exit status: 0 proven, 1 not,
//! 2 usage.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args != ["--prove-gyroid"] {
        eprintln!("usage: {}", fab_sdf_proof::gyroid_proof::PROVE);
        return ExitCode::from(2);
    }
    match fab_sdf_proof::gyroid_proof::prove_and_record() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fab-sdf-proof: the gyroid's declared bounds are not proven: {e}");
            ExitCode::FAILURE
        }
    }
}
