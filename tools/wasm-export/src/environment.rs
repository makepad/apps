//! Browser inputs shared by the exporter and trace host. The selected values
//! are written into each trace; Node uses that record, never a second preset table.
#[derive(Clone, Debug, PartialEq)]
pub struct Environment {
    pub cores: u32,
    pub gpu: String,
    pub jitter: bool,
    pub seed: u32,
    pub path: String,
}

impl Default for Environment {
    fn default() -> Self {
        Self { cores: 4, gpu: "amd".into(), jitter: true, seed: 1, path: "/index.html".into() }
    }
}

impl Environment {
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        match key {
            "cores" => self.cores = value.parse().ok().filter(|n| *n > 0).ok_or("cores must be positive")?,
            "gpu" => { gpu(value)?; self.gpu = value.into(); }
            "clock" => self.jitter = match value { "jitter" => true, "frame" => false, _ => return Err("clock must be jitter or frame".into()) },
            "seed" => self.seed = value.parse().map_err(|_| "seed must be a u32")?,
            "path" if value.starts_with('/') && !value.starts_with("//") && !value.contains(['?', '#', '\\']) && !value.split('/').any(|p| matches!(p, "." | "..")) => self.path = value.into(),
            _ => return Err(format!("invalid environment option {key}={value}")),
        }
        Ok(())
    }

    pub fn args(&self) -> Vec<String> {
        vec!["--cores".into(), self.cores.to_string(), "--gpu".into(), self.gpu.clone(),
            "--clock".into(), if self.jitter { "jitter" } else { "frame" }.into(),
            "--seed".into(), self.seed.to_string(), "--path".into(), self.path.clone()]
    }
}

pub struct Gpu {
    pub vendor: &'static str,
    pub renderer: &'static str,
    pub uniforms: u32,
    pub float_targets: bool,
    pub parallel_compile: bool,
}

/// UNMASKED_VENDOR/RENDERER strings, not GL_VENDOR/RENDERER's masked names.
/// NVIDIA and Apple use representative ANGLE renderers. AMD comes from an Edge capture.
/// Mesa: Mozilla bug 1746306. D3D11 exposes 1024 uniform vectors.
/// Mobile strings, limits and float targets (Adreno and Mali): WebGL2
/// diagnostics collected from real devices.
pub fn gpu(name: &str) -> Result<Gpu, String> {
    let (vendor, renderer, uniforms) = match name {
        "trace" => ("makepad-web-trace", "makepad-web-trace (recorded WebGL 2)", 1024),
        "nvidia" => ("Google Inc. (NVIDIA)", "ANGLE (NVIDIA, NVIDIA RTX A2000 (0x00002531) Direct3D11 vs_5_0 ps_5_0, D3D11-31.0.15.5222)", 1024),
        "amd" => ("Google Inc. (AMD)", "ANGLE (AMD, AMD Radeon(TM) Graphics (0x0000164E) Direct3D11 vs_5_0 ps_5_0, D3D11-32.0.21030.2001)", 1024),
        // Chromium issue 557246812, real GPU diagnostic from the reporter.
        "intel" => ("Google Inc. (Intel)", "ANGLE (Intel, Intel(R) UHD Graphics (0x0000A78B) Direct3D11 vs_5_0 ps_5_0, D3D11-32.0.101.6790)", 1024),
        "apple" => ("Google Inc. (Apple)", "ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)", 1024),
        "mesa" => ("VMware, Inc.", "llvmpipe (LLVM 10.0.0, 128 bits)", 4096),
        "adreno" => ("Qualcomm", "Adreno (TM) 640", 256),
        "mali" => ("ARM", "Mali-G925-Immortalis MC12", 4096),
        _ => return Err(format!("unknown GPU {name}; expected trace, nvidia, amd, intel, apple, mesa, adreno, mali")),
    };
    Ok(Gpu { vendor, renderer, uniforms, float_targets: true, parallel_compile: !matches!(name, "trace" | "adreno" | "mali") })
}
