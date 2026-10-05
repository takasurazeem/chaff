//! What this machine can actually do.
//!
//! The PRD's requirement is that hardware capability is a **runtime property, not an
//! install-time assumption**. A laptop with no GPU must still cull; a machine with a 24 GB
//! card must use it. That means asking, at startup, what is here.
//!
//! # Why the report is plain text and one click away
//!
//! "Why did it pick that model?" must never be a mystery. A tier selection the user cannot
//! inspect is indistinguishable from a bug, so [`render`] produces something a person can
//! read and paste into a bug report.
//!
//! # What this deliberately does not do
//!
//! It does not *load* anything. Reporting which ONNX execution providers actually
//! initialise requires ONNX Runtime, which arrives with the face pipeline (#42). What it
//! reports instead is what the hardware supports, which is the input that decision needs
//! and is answerable today.
//!
//! Every parser here is a pure function over a string, so the interesting logic is tested
//! against real command output captured from real machines rather than against a mock.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

/// A capability class, selecting how large a model the machine can run.
///
/// The ladder from the PRD. Ordered, so `Tier::Remote < Tier::LocalLarge` and a comparison
/// says which is better.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tier {
    /// A configured LAN endpoint is healthy. The largest models, off this machine.
    Remote,
    /// 20 GB or more of VRAM. A 27B-class model at Q4.
    LocalLarge,
    /// 12 to 19 GB. A 7-8B vision model.
    LocalMid,
    /// 6 to 11 GB. A 4B model, or a 2B edge model.
    LocalSmall,
    /// No usable GPU. Faces, sharpness and bursts still work; natural-language tagging
    /// does not, and is queued rather than faked.
    CpuOnly,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Remote => "0 — Remote GPU",
            Tier::LocalLarge => "1 — Local, large",
            Tier::LocalMid => "2 — Local, mid",
            Tier::LocalSmall => "3 — Local, small",
            Tier::CpuOnly => "4 — CPU only",
        }
    }

    /// Whether this tier can run a vision language model at all.
    ///
    /// Tier 4 cannot, and says so rather than silently producing worse tags from a model
    /// that was never going to fit. What it *can* do — faces, sharpness, bursts, exposure —
    /// is unaffected, which is the point of the ladder.
    pub fn has_vision_model(self) -> bool {
        self != Tier::CpuOnly
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Apple,
    Intel,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gpu {
    pub vendor: GpuVendor,
    pub name: String,
    /// Reported VRAM, when the vendor tool reports it. Apple Silicon reports unified
    /// memory rather than a dedicated figure, and `None` is honest there.
    pub vram_bytes: Option<u64>,
    pub driver: Option<String>,
}

impl Gpu {
    pub fn vram_gb(&self) -> Option<f64> {
        self.vram_bytes.map(|b| b as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// A configured model endpoint and whether it answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointHealth {
    pub url: String,
    pub healthy: bool,
    /// What came back, for the report. Empty when it did not answer.
    pub detail: String,
}

/// Everything the probe found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub os: String,
    pub arch: String,
    pub cpu_brand: String,
    pub logical_cores: usize,
    pub simd: Vec<String>,
    pub ram_total_bytes: u64,
    pub ram_available_bytes: u64,
    /// Free space on the volume holding the app data directory.
    pub disk_available_bytes: u64,
    pub gpus: Vec<Gpu>,
    pub endpoints: Vec<EndpointHealth>,
}

/// The share of unified memory a GPU may use on Apple Silicon.
///
/// Apple's own guidance is that a Metal application may use roughly 70% of unified memory,
/// and `recommendedMaxWorkingSetSize` on a 16 GB Mac comes out near 10.6 GB. Using the
/// whole total would classify a 16 GB laptop as a 16 GB workstation, which is the mistake
/// in the other direction.
const APPLE_UNIFIED_FRACTION: f64 = 0.70;

impl Probe {
    /// The largest usable GPU memory figure among the GPUs found.
    ///
    /// For a GPU that reports its own VRAM, that figure. For Apple Silicon, which shares
    /// memory with the system and reports none, a fraction of unified memory.
    ///
    /// This distinction was found by running the probe on a real M1 Pro, which came back
    /// as **Tier 4 — CPU only**. The ladder is VRAM-based, which is an NVIDIA-centric
    /// framing, and a machine with a 16-core Metal GPU and 16 GB of shared memory fell
    /// through it entirely. The user would have been told their laptop could not run a
    /// vision model when a 4B one fits comfortably.
    ///
    /// A GPU from any other vendor that reports no memory stays `None`: an integrated
    /// Intel GPU genuinely cannot run a vision model usefully, and guessing a figure for
    /// it would be the same mistake in reverse.
    pub fn best_vram_bytes(&self) -> Option<u64> {
        // **A measured figure always beats an estimate**, whatever their sizes. Taking the
        // maximum across both would let a 128 GB Mac's shared-memory estimate outrank a
        // discrete card's reported 12 GB, which is the wrong way round: one number was
        // read from the hardware and the other was inferred from a fraction.
        if let Some(reported) = self.gpus.iter().filter_map(|g| g.vram_bytes).max() {
            return Some(reported);
        }

        // Nothing reported a figure. Apple Silicon is the only case where that is not the
        // end of the story, because it shares memory with the system rather than having
        // none.
        self.gpus
            .iter()
            .filter(|g| g.vendor == GpuVendor::Apple)
            .map(|_| (self.ram_total_bytes as f64 * APPLE_UNIFIED_FRACTION) as u64)
            .max()
    }

    /// A healthy endpoint, if any.
    pub fn healthy_endpoint(&self) -> Option<&EndpointHealth> {
        self.endpoints.iter().find(|e| e.healthy)
    }
}

/// Choose a tier from a probe.
///
/// Pure, so the ladder is tested against fabricated hardware rather than against whatever
/// this machine happens to have. A tier selection that can only be tested on one machine is
/// a tier selection with four untested branches.
pub fn choose_tier(probe: &Probe) -> (Tier, String) {
    // A healthy remote endpoint wins outright: it is strictly more capable than anything
    // local, and it costs this machine nothing.
    if let Some(e) = probe.healthy_endpoint() {
        return (
            Tier::Remote,
            format!("{} answered and reports itself healthy", e.url),
        );
    }

    let Some(bytes) = probe.best_vram_bytes() else {
        return (
            Tier::CpuOnly,
            "no GPU with a reported memory size; faces, sharpness and bursts run on the \
             CPU, and natural-language tagging is queued rather than attempted"
                .to_string(),
        );
    };

    const GB: u64 = 1024 * 1024 * 1024;
    let gb = bytes as f64 / GB as f64;
    // Name the card the figure actually came from, and say which kind of figure it is —
    // "reports 24 GB of VRAM" and "may use about 11 GB of shared memory" are different
    // claims and the user should be able to tell them apart.
    let source = probe
        .gpus
        .iter()
        .find(|g| g.vram_bytes == Some(bytes))
        .map(|g| (g.name.as_str(), format!("{gb:.0} GB of VRAM")))
        .or_else(|| {
            probe
                .gpus
                .iter()
                .find(|g| g.vendor == GpuVendor::Apple && g.vram_bytes.is_none())
                .map(|g| {
                    (
                        g.name.as_str(),
                        format!("about {gb:.0} GB of shared memory it may use"),
                    )
                })
        })
        .unwrap_or(("a GPU", format!("{gb:.0} GB")));
    let (which, figure) = source;

    if bytes >= 20 * GB {
        (Tier::LocalLarge, format!("{which} offers {figure}"))
    } else if bytes >= 12 * GB {
        (Tier::LocalMid, format!("{which} offers {figure}"))
    } else if bytes >= 6 * GB {
        (Tier::LocalSmall, format!("{which} offers {figure}"))
    } else {
        (
            Tier::CpuOnly,
            format!(
                "{which} offers only {figure}, below the 6 GB floor for a vision model; \
                 running on the CPU instead"
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// Parsers — pure, so they are tested against real command output
// ---------------------------------------------------------------------------
/// Parse `nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader,nounits`.
///
/// Sample line: `NVIDIA GeForce RTX 3090, 24576, 595.104.02`
///
/// MiB, not MB — nvidia-smi reports mebibytes. Treating them as megabytes understates a
/// 24 GB card by 4%, which is not enough to change a tier but is enough to make a report
/// that disagrees with `nvidia-smi` look wrong.
pub fn parse_nvidia_smi(output: &str) -> Vec<Gpu> {
    output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split(',').map(str::trim).collect();
            if parts.len() < 2 || parts[0].is_empty() {
                return None;
            }
            let vram_bytes = parts[1]
                .parse::<u64>()
                .ok()
                .map(|mib| mib * 1024 * 1024);
            Some(Gpu {
                vendor: GpuVendor::Nvidia,
                name: parts[0].to_string(),
                vram_bytes,
                driver: parts.get(2).map(|s| s.to_string()).filter(|s| !s.is_empty()),
            })
        })
        .collect()
}

/// Parse the GPU section of `system_profiler SPDisplaysDataType`.
///
/// Looks for `Chipset Model:` and, when present, `VRAM (Total):`. Apple Silicon reports no
/// VRAM line because the memory is shared, and `None` is the honest answer there rather
/// than a guess at the unified total.
pub fn parse_system_profiler(output: &str) -> Vec<Gpu> {
    let mut gpus = Vec::new();
    let mut current: Option<Gpu> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Chipset Model:") {
            if let Some(g) = current.take() {
                gpus.push(g);
            }
            let name = rest.trim().to_string();
            let vendor = classify_vendor(&name);
            current = Some(Gpu { vendor, name, vram_bytes: None, driver: None });
        } else if let Some(rest) = trimmed.strip_prefix("VRAM (Total):") {
            if let Some(g) = current.as_mut() {
                g.vram_bytes = parse_vram(rest.trim());
            }
        } else if let Some(rest) = trimmed.strip_prefix("VRAM (Dynamic, Max):") {
            // Apple Silicon's integrated GPUs report this instead.
            if let Some(g) = current.as_mut() {
                g.vram_bytes = parse_vram(rest.trim());
            }
        }
    }
    if let Some(g) = current {
        gpus.push(g);
    }
    gpus
}

fn classify_vendor(name: &str) -> GpuVendor {
    let n = name.to_ascii_lowercase();
    if n.contains("nvidia") || n.contains("geforce") || n.contains("quadro") || n.contains("rtx") {
        GpuVendor::Nvidia
    } else if n.contains("amd") || n.contains("radeon") {
        GpuVendor::Amd
    } else if n.contains("apple") || n.contains("m1") || n.contains("m2") || n.contains("m3") || n.contains("m4") {
        GpuVendor::Apple
    } else if n.contains("intel") || n.contains("iris") || n.contains("uhd graphics") {
        GpuVendor::Intel
    } else {
        GpuVendor::Unknown
    }
}

/// `"24 GB"`, `"1536 MB"`, `"8 GB"` -> bytes.
fn parse_vram(text: &str) -> Option<u64> {
    let mut parts = text.split_whitespace();
    let value: f64 = parts.next()?.parse().ok()?;
    let unit = parts.next()?.to_ascii_lowercase();
    let multiplier = match unit.trim_end_matches('b') {
        "g" => 1024u64 * 1024 * 1024,
        "m" => 1024 * 1024,
        "k" => 1024,
        _ => return None,
    };
    Some((value * multiplier as f64) as u64)
}

// ---------------------------------------------------------------------------
// Probing
// ---------------------------------------------------------------------------
/// Ask this machine what it has.
pub fn probe() -> Probe {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();

    let cpu_brand = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .unwrap_or_default();

    Probe {
        os: probe_os(),
        arch: std::env::consts::ARCH.to_string(),
        cpu_brand,
        logical_cores: sys.cpus().len().max(
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        ),
        simd: probe_simd(),
        ram_total_bytes: sys.total_memory(),
        ram_available_bytes: sys.available_memory(),
        disk_available_bytes: probe_disk(),
        gpus: probe_gpus(),
        endpoints: Vec::new(),
    }
}

fn probe_os() -> String {
    let name = sysinfo::System::name().unwrap_or_else(|| std::env::consts::OS.to_string());
    match sysinfo::System::os_version() {
        Some(v) => format!("{name} {v}"),
        None => name,
    }
}

/// CPU features that matter for this workload.
///
/// Not a general CPU report: these are the ones that decide whether image decoding and
/// matrix maths take the fast path.
fn probe_simd() -> Vec<String> {
    let mut out = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            out.push("AVX2".to_string());
        }
        if std::is_x86_feature_detected!("avx512f") {
            out.push("AVX-512".to_string());
        }
        if std::is_x86_feature_detected!("fma") {
            out.push("FMA".to_string());
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // NEON is baseline on aarch64, so its presence is not informative; what matters
        // for inference is the half-precision path.
        out.push("NEON".to_string());
        if std::arch::is_aarch64_feature_detected!("fp16") {
            out.push("FP16".to_string());
        }
    }
    out
}

fn probe_disk() -> u64 {
    let mount = sysinfo::Disks::new_with_refreshed_list();
    // Prefer the volume holding the working directory: that is where a catalog and a
    // thumbnail cache would go, and it is the one that runs out.
    let here = std::env::current_dir().unwrap_or_else(|_| Path::new("/").to_path_buf());
    let best = mount
        .list()
        .iter()
        .filter(|d| here.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .or_else(|| mount.list().first());
    best.map(|d| d.available_space()).unwrap_or(0)
}

fn probe_gpus() -> Vec<Gpu> {
    // nvidia-smi first: it is the only tool that reports VRAM reliably, and it exists on
    // Linux and Windows wherever an NVIDIA driver is installed.
    if let Some(out) = run("nvidia-smi", &[
        "--query-gpu=name,memory.total,driver_version",
        "--format=csv,noheader,nounits",
    ]) {
        let gpus = parse_nvidia_smi(&out);
        if !gpus.is_empty() {
            return gpus;
        }
    }

    // macOS, including Apple Silicon where there is no separate VRAM figure.
    if let Some(out) = run("system_profiler", &["SPDisplaysDataType"]) {
        let gpus = parse_system_profiler(&out);
        if !gpus.is_empty() {
            return gpus;
        }
    }

    Vec::new()
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Probe a configured endpoint for health.
///
/// Deliberately a plain TCP-ish HTTP GET with a short timeout rather than a full client:
/// this runs at startup and must not delay the window. The point is "is something there",
/// not "is it healthy in every respect".
pub fn probe_endpoint(url: &str, timeout_ms: u64) -> EndpointHealth {
    let base = url.trim_end_matches('/');
    let health = format!("{base}/health");

    // A minimal HTTP/1.0 request over a raw socket, so this adds no dependency and cannot
    // be defeated by a proxy or a redirect.
    let parsed = base
        .strip_prefix("http://")
        .and_then(|rest| rest.split('/').next());
    let Some(hostport) = parsed else {
        return EndpointHealth {
            url: url.to_string(),
            healthy: false,
            detail: "only http:// endpoints are probed".to_string(),
        };
    };

    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let Ok(mut stream) = TcpStream::connect_timeout(
        &match hostport.to_socket_addrs_first() {
            Some(a) => a,
            None => {
                return EndpointHealth {
                    url: url.to_string(),
                    healthy: false,
                    detail: format!("could not resolve {hostport}"),
                }
            }
        },
        Duration::from_millis(timeout_ms),
    ) else {
        return EndpointHealth { url: url.to_string(), healthy: false, detail: "no connection".to_string() };
    };

    let _ = stream.set_read_timeout(Some(Duration::from_millis(timeout_ms)));
    let path = health.strip_prefix(&format!("http://{hostport}")).unwrap_or("/health");
    let request = format!("GET {path} HTTP/1.0\r\nHost: {hostport}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return EndpointHealth { url: url.to_string(), healthy: false, detail: "write failed".to_string() };
    }

    let mut buf = String::new();
    let _ = stream.read_to_string(&mut buf);
    let healthy = buf.starts_with("HTTP/1.") && buf.contains(" 200");

    EndpointHealth {
        url: url.to_string(),
        healthy,
        detail: buf.lines().next().unwrap_or("no response").trim().to_string(),
    }
}

/// A tiny helper so the socket code above reads linearly.
trait HostPort {
    fn to_socket_addrs_first(&self) -> Option<std::net::SocketAddr>;
}

impl HostPort for &str {
    fn to_socket_addrs_first(&self) -> Option<std::net::SocketAddr> {
        use std::net::ToSocketAddrs;
        (*self).to_socket_addrs().ok()?.next()
    }
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------
/// A plain-text Capability Report.
///
/// The user must be able to see why a tier was chosen. A selection they cannot inspect is
/// indistinguishable from a bug.
pub fn render(probe: &Probe) -> String {
    render_with_build(probe, None)
}

/// The same, naming which build produced it.
///
/// The build stamp is passed in rather than read here, because `chaff-core` has no build
/// script and no business knowing about one. The shell supplies it; the engine just prints
/// it. "Which build is this?" is the first question when something behaves oddly, and the
/// report is where someone already looks.
pub fn render_with_build(probe: &Probe, build: Option<&str>) -> String {
    let (tier, reason) = choose_tier(probe);
    let gb = |b: u64| b as f64 / (1024.0 * 1024.0 * 1024.0);

    let mut out = String::new();
    out.push_str("Chaff — Capability Report\n");
    out.push_str("=========================\n\n");
    if let Some(b) = build {
        out.push_str(&format!("Build       {b}\n\n"));
    }

    out.push_str(&format!("System      {} ({})\n", probe.os, probe.arch));
    out.push_str(&format!(
        "CPU         {}\n            {} logical cores, {}\n",
        if probe.cpu_brand.is_empty() { "unknown" } else { &probe.cpu_brand },
        probe.logical_cores,
        if probe.simd.is_empty() { "no notable SIMD".to_string() } else { probe.simd.join(", ") }
    ));
    out.push_str(&format!(
        "Memory      {:.1} GB total, {:.1} GB available\n",
        gb(probe.ram_total_bytes),
        gb(probe.ram_available_bytes)
    ));
    out.push_str(&format!("Disk free   {:.1} GB\n", gb(probe.disk_available_bytes)));

    out.push('\n');
    if probe.gpus.is_empty() {
        out.push_str("GPU         none detected\n");
    } else {
        for g in &probe.gpus {
            out.push_str(&format!(
                "GPU         {} ({:?}){} {}\n",
                g.name,
                g.vendor,
                match g.vram_gb() {
                    Some(v) => format!(" — {v:.1} GB"),
                    None => " — memory shared with the system".to_string(),
                },
                g.driver.as_deref().map(|d| format!("driver {d}")).unwrap_or_default()
            ));
        }
    }

    if !probe.endpoints.is_empty() {
        out.push('\n');
        for e in &probe.endpoints {
            out.push_str(&format!(
                "Endpoint    {} — {}\n",
                e.url,
                if e.healthy { format!("healthy ({})", e.detail) } else { format!("unreachable ({})", e.detail) }
            ));
        }
    }

    out.push_str(&format!("\nTier        {}\n", tier.label()));
    out.push_str(&format!("Because     {reason}\n"));

    if !tier.has_vision_model() {
        out.push_str(
            "\nAt this tier Chaff still pairs RAW and JPEG, scores focus, exposure and\n\
             noise, groups bursts and detects brackets — all on the CPU. Natural-language\n\
             tagging is queued rather than attempted with a model that would not fit.\n",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    fn empty_probe() -> Probe {
        Probe {
            os: "Test".into(),
            arch: "x86_64".into(),
            cpu_brand: "Test CPU".into(),
            logical_cores: 8,
            simd: vec!["AVX2".into()],
            ram_total_bytes: 16 * GB,
            ram_available_bytes: 8 * GB,
            disk_available_bytes: 100 * GB,
            gpus: Vec::new(),
            endpoints: Vec::new(),
        }
    }

    fn with_gpu(vram_gb: u64) -> Probe {
        let mut p = empty_probe();
        p.gpus.push(Gpu {
            vendor: GpuVendor::Nvidia,
            name: format!("Test GPU {vram_gb}GB"),
            vram_bytes: Some(vram_gb * GB),
            driver: Some("1.2.3".into()),
        });
        p
    }

    // ---------------------------------------------------------------------
    // The ladder
    // ---------------------------------------------------------------------
    #[test]
    fn the_tier_ladder_follows_the_documented_thresholds() {
        // Every branch, against fabricated hardware. A tier selection that can only be
        // tested on one machine has four untested branches.
        assert_eq!(choose_tier(&with_gpu(24)).0, Tier::LocalLarge);
        assert_eq!(choose_tier(&with_gpu(20)).0, Tier::LocalLarge, "the boundary is inclusive");
        assert_eq!(choose_tier(&with_gpu(19)).0, Tier::LocalMid);
        assert_eq!(choose_tier(&with_gpu(12)).0, Tier::LocalMid);
        assert_eq!(choose_tier(&with_gpu(11)).0, Tier::LocalSmall);
        assert_eq!(choose_tier(&with_gpu(6)).0, Tier::LocalSmall);
        assert_eq!(choose_tier(&with_gpu(4)).0, Tier::CpuOnly, "below the 6 GB floor");
        assert_eq!(choose_tier(&empty_probe()).0, Tier::CpuOnly, "no GPU at all");
    }

    #[test]
    fn a_healthy_remote_endpoint_wins_over_any_local_gpu() {
        // Strictly more capable, and it costs this machine nothing.
        let mut p = with_gpu(24);
        p.endpoints.push(EndpointHealth {
            url: "http://192.0.2.10:8080".into(),
            healthy: true,
            detail: "HTTP/1.1 200 OK".into(),
        });
        let (tier, reason) = choose_tier(&p);
        assert_eq!(tier, Tier::Remote);
        assert!(reason.contains("192.0.2.10"), "the reason must name the endpoint");
    }

    #[test]
    fn an_unhealthy_endpoint_does_not_select_the_remote_tier() {
        let mut p = with_gpu(8);
        p.endpoints.push(EndpointHealth {
            url: "http://192.0.2.10:8080".into(),
            healthy: false,
            detail: "no connection".into(),
        });
        assert_eq!(choose_tier(&p).0, Tier::LocalSmall, "a dead endpoint is not a tier");
    }

    #[test]
    fn the_largest_gpu_decides_when_there_are_several() {
        let mut p = empty_probe();
        p.gpus.push(Gpu { vendor: GpuVendor::Intel, name: "iGPU".into(), vram_bytes: Some(2 * GB), driver: None });
        p.gpus.push(Gpu { vendor: GpuVendor::Nvidia, name: "RTX 3090".into(), vram_bytes: Some(24 * GB), driver: None });
        let (tier, reason) = choose_tier(&p);
        assert_eq!(tier, Tier::LocalLarge);
        assert!(reason.contains("RTX 3090"), "the reason must name the card it chose: {reason}");
    }

    #[test]
    fn apple_silicon_is_classified_by_its_shared_memory_not_as_cpu_only() {
        // **The bug this test used to encode.** It asserted that a GPU with no reported
        // VRAM means CPU only, which classified a real M1 Pro — a 16-core Metal GPU with
        // 16 GB of unified memory — as unable to run a vision model at all.
        //
        // Apple's own guidance is that a Metal application may use roughly 70% of unified
        // memory, so that is the figure used, and the reason names it as shared rather
        // than claiming a dedicated number.
        let mut p = empty_probe();
        p.ram_total_bytes = 16 * GB;
        p.gpus.push(Gpu { vendor: GpuVendor::Apple, name: "Apple M1 Pro".into(), vram_bytes: None, driver: None });

        let (tier, reason) = choose_tier(&p);
        assert_eq!(tier, Tier::LocalSmall, "16 GB of shared memory fits a small model");
        assert!(tier.has_vision_model(), "a laptop with a Metal GPU is not CPU only");
        assert!(reason.contains("shared memory"), "the claim must be accurate: {reason}");
        assert!(!reason.contains("VRAM"), "it must not claim a dedicated figure: {reason}");
    }

    #[test]
    fn a_larger_apple_machine_reaches_a_higher_tier() {
        // 32 GB of unified memory is 22 GB usable, which is tier 1.
        let mut p = empty_probe();
        p.ram_total_bytes = 32 * GB;
        p.gpus.push(Gpu { vendor: GpuVendor::Apple, name: "Apple M2 Max".into(), vram_bytes: None, driver: None });
        assert_eq!(choose_tier(&p).0, Tier::LocalLarge);
    }

    #[test]
    fn an_integrated_gpu_from_another_vendor_stays_cpu_only() {
        // The same mistake in reverse. An Intel iGPU that reports no memory genuinely
        // cannot run a vision model, and guessing a figure for it would be as wrong as
        // classifying the Mac as CPU only.
        let mut p = empty_probe();
        p.ram_total_bytes = 32 * GB;
        p.gpus.push(Gpu { vendor: GpuVendor::Intel, name: "Intel UHD Graphics 630".into(), vram_bytes: None, driver: None });
        assert_eq!(choose_tier(&p).0, Tier::CpuOnly);
    }

    #[test]
    fn a_measured_figure_always_beats_an_estimated_one() {
        // Whatever their sizes. The first version of this took the maximum across both,
        // which let a 128 GB Mac's shared-memory estimate outrank a discrete card's
        // reported 12 GB — one number read from hardware losing to one inferred from a
        // fraction.
        let mut p = empty_probe();
        p.ram_total_bytes = 128 * GB;
        p.gpus.push(Gpu { vendor: GpuVendor::Apple, name: "Apple M2 Ultra".into(), vram_bytes: None, driver: None });
        p.gpus.push(Gpu { vendor: GpuVendor::Nvidia, name: "RTX 3060".into(), vram_bytes: Some(12 * GB), driver: None });
        assert_eq!(p.best_vram_bytes(), Some(12 * GB));
        assert_eq!(choose_tier(&p).0, Tier::LocalMid);
    }

    #[test]
    fn every_tier_has_a_label_and_declares_whether_it_can_run_a_vision_model() {
        for t in [Tier::Remote, Tier::LocalLarge, Tier::LocalMid, Tier::LocalSmall, Tier::CpuOnly] {
            assert!(!t.label().is_empty());
        }
        assert!(Tier::Remote.has_vision_model());
        assert!(Tier::LocalSmall.has_vision_model());
        assert!(!Tier::CpuOnly.has_vision_model());
    }

    #[test]
    fn tiers_are_ordered_from_best_to_worst() {
        // The ordering is what makes `tier >= Tier::LocalMid` mean anything.
        assert!(Tier::Remote < Tier::LocalLarge);
        assert!(Tier::LocalLarge < Tier::LocalMid);
        assert!(Tier::LocalMid < Tier::LocalSmall);
        assert!(Tier::LocalSmall < Tier::CpuOnly);
    }

    // ---------------------------------------------------------------------
    // nvidia-smi parsing
    // ---------------------------------------------------------------------
    #[test]
    fn parses_real_nvidia_smi_output() {
        // Captured from a real machine running an RTX 3090.
        let out = "NVIDIA GeForce RTX 3090, 24576, 595.104.02\n";
        let gpus = parse_nvidia_smi(out);
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 3090");
        assert_eq!(gpus[0].vendor, GpuVendor::Nvidia);
        assert_eq!(gpus[0].driver.as_deref(), Some("595.104.02"));
        // 24576 MiB, not MB.
        assert_eq!(gpus[0].vram_bytes, Some(24576 * 1024 * 1024));
        assert!((gpus[0].vram_gb().unwrap() - 24.0).abs() < 0.01);
    }

    #[test]
    fn a_24gb_card_lands_in_the_large_tier_from_real_output() {
        // The end-to-end check: real command output, through the parser, into the ladder.
        let gpus = parse_nvidia_smi("NVIDIA GeForce RTX 3090, 24576, 595.104.02\n");
        let mut p = empty_probe();
        p.gpus = gpus;
        assert_eq!(choose_tier(&p).0, Tier::LocalLarge);
    }

    #[test]
    fn parses_several_cards() {
        let out = "NVIDIA GeForce RTX 3090, 24576, 595.104.02\nNVIDIA GeForce RTX 3060, 12288, 595.104.02\n";
        let gpus = parse_nvidia_smi(out);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[1].name, "NVIDIA GeForce RTX 3060");
        assert_eq!(gpus[1].vram_bytes, Some(12288 * 1024 * 1024));
    }

    #[test]
    fn tolerates_missing_and_malformed_columns() {
        assert!(parse_nvidia_smi("").is_empty());
        assert!(parse_nvidia_smi("\n\n").is_empty());
        // A card whose memory could not be read still has a name worth reporting.
        let gpus = parse_nvidia_smi("NVIDIA GeForce RTX 3090, [N/A], 595.104.02\n");
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].vram_bytes, None);
        // A line with no comma at all is not a card.
        assert!(parse_nvidia_smi("garbage without commas").is_empty());
    }

    // ---------------------------------------------------------------------
    // system_profiler parsing
    // ---------------------------------------------------------------------
    #[test]
    fn parses_real_system_profiler_output() {
        let out = "\
Graphics/Displays:

    Apple M1 Pro:

      Chipset Model: Apple M1 Pro
      Type: GPU
      Bus: Built-In
      Total Number of Cores: 16
      Vendor: Apple (0x106b)
      Metal Support: Metal 3
";
        let gpus = parse_system_profiler(out);
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "Apple M1 Pro");
        assert_eq!(gpus[0].vendor, GpuVendor::Apple);
        assert_eq!(gpus[0].vram_bytes, None, "unified memory is not a dedicated figure");
    }

    #[test]
    fn parses_a_discrete_gpu_with_vram() {
        let out = "\
    NVIDIA GeForce RTX 3090:

      Chipset Model: NVIDIA GeForce RTX 3090
      VRAM (Total): 24 GB
      Vendor: NVIDIA (0x10de)
";
        let gpus = parse_system_profiler(out);
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].vram_bytes, Some(24 * GB));
        assert_eq!(gpus[0].vendor, GpuVendor::Nvidia);
    }

    #[test]
    fn parses_two_gpus_from_one_report() {
        let out = "\
      Chipset Model: Intel UHD Graphics 630
      VRAM (Dynamic, Max): 1536 MB
      Chipset Model: AMD Radeon Pro 5500M
      VRAM (Total): 8 GB
";
        let gpus = parse_system_profiler(out);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].vendor, GpuVendor::Intel);
        assert_eq!(gpus[0].vram_bytes, Some(1536 * 1024 * 1024));
        assert_eq!(gpus[1].vendor, GpuVendor::Amd);
        assert_eq!(gpus[1].vram_bytes, Some(8 * GB));
    }

    #[test]
    fn parses_vram_units() {
        assert_eq!(parse_vram("24 GB"), Some(24 * GB));
        assert_eq!(parse_vram("1536 MB"), Some(1536 * 1024 * 1024));
        assert_eq!(parse_vram("8 GB"), Some(8 * GB));
        assert_eq!(parse_vram("nonsense"), None);
        assert_eq!(parse_vram(""), None);
    }

    #[test]
    fn a_report_with_no_gpu_yields_nothing() {
        assert!(parse_system_profiler("Graphics/Displays:\n\n    Something else:\n").is_empty());
    }

    // ---------------------------------------------------------------------
    // The report
    // ---------------------------------------------------------------------
    #[test]
    fn the_build_stamp_appears_when_supplied() {
        // "Which build is this?" is the first question when something behaves oddly, and
        // the user once spent time testing a binary that was seven minutes stale without
        // any way to tell.
        let text = render_with_build(&with_gpu(24), Some("0.1.0 (abc1234, 2026-10-04 21:46 UTC)"));
        assert!(text.contains("Build"));
        assert!(text.contains("abc1234"));
    }

    #[test]
    fn the_report_still_renders_without_a_build_stamp() {
        let text = render(&with_gpu(24));
        assert!(text.contains("Capability Report"));
        assert!(!text.contains("Build       "), "no stamp supplied, none printed");
    }

    #[test]
    fn the_report_states_the_tier_and_the_reason() {
        // A tier the user cannot inspect is indistinguishable from a bug.
        let text = render(&with_gpu(24));
        assert!(text.contains("Tier"), "the report must name a tier:\n{text}");
        assert!(text.contains("Because"), "and say why:\n{text}");
        assert!(text.contains("Test GPU 24GB"), "and name the hardware it chose:\n{text}");
    }

    #[test]
    fn the_cpu_only_report_says_what_still_works() {
        // The tier that loses a feature must say what it keeps, or a laptop user reads
        // "CPU only" as "this application does not work here".
        let text = render(&empty_probe());
        assert!(text.contains("CPU only"));
        assert!(text.contains("bursts"), "it must name what still works:\n{text}");
        assert!(text.contains("queued"), "and be honest about what does not:\n{text}");
    }

    #[test]
    fn the_report_renders_for_every_tier_without_panicking() {
        for p in [empty_probe(), with_gpu(24), with_gpu(8), with_gpu(2)] {
            let text = render(&p);
            assert!(!text.is_empty());
            assert!(text.contains("Capability Report"));
        }
    }

    #[test]
    fn an_unreachable_endpoint_is_reported_as_unreachable() {
        let mut p = empty_probe();
        p.endpoints.push(EndpointHealth {
            url: "http://192.0.2.10:8080".into(),
            healthy: false,
            detail: "no connection".into(),
        });
        let text = render(&p);
        assert!(text.contains("unreachable"));
        assert!(text.contains("192.0.2.10"));
    }

    // ---------------------------------------------------------------------
    // Live probing
    // ---------------------------------------------------------------------
    #[test]
    fn probing_this_machine_produces_a_usable_report() {
        // Whatever this machine is, the probe must return something coherent. The
        // assertions are deliberately weak — they hold on any hardware — because a test
        // that asserted "there is a 3090 here" would fail on every other machine.
        let p = probe();
        assert!(!p.os.is_empty());
        assert!(!p.arch.is_empty());
        assert!(p.logical_cores >= 1);
        assert!(p.ram_total_bytes > 0, "a machine with no RAM is a probe that failed");
        assert!(!render(&p).is_empty());
    }

    #[test]
    fn probing_an_endpoint_that_is_not_there_fails_cleanly() {
        // Port 1 is reserved and never listening. The probe must return, not hang.
        let started = std::time::Instant::now();
        let e = probe_endpoint("http://127.0.0.1:1", 300);
        assert!(!e.healthy);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the probe must respect its timeout, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_non_http_endpoint_is_reported_rather_than_attempted() {
        let e = probe_endpoint("https://example.com", 300);
        assert!(!e.healthy);
        assert!(e.detail.contains("http://"));
    }
}
