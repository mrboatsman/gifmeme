//! ffmpeg pipeline: anything ffmpeg can decode (gif, webp, png, jpg, mp4,
//! webm, ...) -> (animated) WebP, at most `max_px` on the long side, alpha
//! preserved, quality stepped down until the file fits `target_bytes`.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use tokio::process::Command;

/// Longest clip we keep; Telegram video stickers are <= 3 s, memes rarely > 10 s.
const MAX_SECONDS: &str = "10";
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(180);

pub struct Converted {
    pub bytes: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Deserialize)]
struct Probe {
    #[serde(default)]
    streams: Vec<ProbeStream>,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    nb_read_packets: Option<String>,
    codec_name: Option<String>,
}

struct SourceInfo {
    w: u32,
    h: u32,
    frames: u64,
    fps: f64,
}

fn parse_rate(r: &str) -> Option<f64> {
    let (n, d) = r.split_once('/')?;
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (d > 0.0 && n > 0.0).then_some(n / d)
}

async fn run(cmd: &mut Command) -> Result<std::process::Output> {
    cmd.kill_on_drop(true);
    let out = tokio::time::timeout(FFMPEG_TIMEOUT, cmd.output())
        .await
        .map_err(|_| anyhow!("ffmpeg timed out"))?
        .context("spawning ffmpeg/ffprobe")?;
    Ok(out)
}

/// Last stderr line without the (temp file) path ffmpeg prefixes it with.
fn last_error(stderr: &[u8], input: &Path, fallback: &str) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text.trim().lines().last().unwrap_or(fallback);
    let path = input.display().to_string();
    line.replace(&format!("{path}: "), "").replace(&path, "upload")
}

async fn probe(input: &Path) -> Result<SourceInfo> {
    let out = run(Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-count_packets"])
        .args([
            "-show_entries",
            "stream=width,height,avg_frame_rate,r_frame_rate,nb_read_packets,codec_name",
            "-of",
            "json",
        ])
        .arg(input))
    .await?;
    if !out.status.success() {
        bail!("unsupported file format ({})", last_error(&out.stderr, input, "ffprobe failed"));
    }
    let p: Probe = serde_json::from_slice(&out.stdout).context("parsing ffprobe output")?;
    let s = p.streams.into_iter().next().ok_or_else(|| anyhow!("the file contains no image or video"))?;
    let (w, h) = match (s.width, s.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
        _ => bail!("could not read the image size ({})", s.codec_name.unwrap_or_default()),
    };
    let frames = s.nb_read_packets.and_then(|n| n.parse().ok()).unwrap_or(1);
    let fps = s
        .avg_frame_rate
        .as_deref()
        .and_then(parse_rate)
        .or_else(|| s.r_frame_rate.as_deref().and_then(parse_rate))
        .unwrap_or(10.0);
    Ok(SourceInfo { w, h, frames, fps })
}

struct Attempt {
    px: u32,
    fps: f64,
    quality: u32,
}

// compression_level 4: level 6 costs 30-50x the encode time for ~3% smaller
// files (measured on a 6 s 720p clip: 79 s vs 1.7 s), so step quality,
// frame rate and size down instead.
const LADDER: &[Attempt] = &[
    Attempt { px: 512, fps: 20.0, quality: 80 },
    Attempt { px: 512, fps: 15.0, quality: 70 },
    Attempt { px: 448, fps: 15.0, quality: 60 },
    Attempt { px: 384, fps: 12.0, quality: 55 },
    Attempt { px: 320, fps: 12.0, quality: 50 },
    Attempt { px: 256, fps: 10.0, quality: 45 },
    Attempt { px: 256, fps: 8.0, quality: 40 },
];

async fn encode(input: &Path, output: &Path, src: &SourceInfo, a: &Attempt, max_px: u32) -> Result<Vec<u8>> {
    let animated = src.frames > 1;
    let px = a.px.min(max_px);
    let mut vf = Vec::new();
    if animated && src.fps > a.fps + 0.5 {
        vf.push(format!("fps=fps={}", a.fps));
    }
    // min(iw,px) avoids upscaling small images; decrease keeps aspect.
    vf.push(format!(
        "scale=w='min(iw,{px})':h='min(ih,{px})':force_original_aspect_ratio=decrease:flags=lanczos"
    ));
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
        .arg(input)
        .args(["-t", MAX_SECONDS, "-an", "-sn", "-map_metadata", "-1"])
        .args(["-vf", &vf.join(",")]);
    if animated {
        cmd.args(["-c:v", "libwebp_anim", "-loop", "0"]);
    } else {
        cmd.args(["-c:v", "libwebp", "-frames:v", "1"]);
    }
    cmd.args(["-quality", &a.quality.to_string(), "-compression_level", "4", "-f", "webp"])
        .arg(output);
    let out = run(&mut cmd).await?;
    if !out.status.success() {
        bail!("conversion failed: {}", last_error(&out.stderr, input, "ffmpeg failed"));
    }
    tokio::fs::read(output).await.context("reading ffmpeg output")
}

/// Convert `input` to WebP. `tmp_dir` holds the intermediate output file.
pub async fn to_webp(input: &Path, tmp_dir: &Path, max_px: u32, target_bytes: usize) -> Result<Converted> {
    // Already a small-enough WebP (e.g. exported Telegram/Matrix stickers):
    // keep it byte for byte. ffmpeg cannot decode animated WebP everywhere.
    let head = tokio::fs::read(input).await.context("reading upload")?;
    if let Some((w, h)) = webp_dims(&head) {
        if w <= max_px && h <= max_px && head.len() <= target_bytes {
            return Ok(Converted { bytes: head, w, h });
        }
        // ffmpeg (7.x and 8.0) has no animated WebP decoder.
        if webp_animated(&head) {
            bail!(
                "animated WebP of {w}×{h} / {} kB cannot be resized (max {max_px} px, {} kB), upload it as gif or mp4",
                head.len() / 1000,
                target_bytes / 1000
            );
        }
    }
    drop(head);

    let src = probe(input).await?;
    let out_file = tempfile::Builder::new().suffix(".webp").tempfile_in(tmp_dir)?;
    let mut best: Option<Vec<u8>> = None;
    for a in LADDER {
        let bytes = encode(input, out_file.path(), &src, a, max_px).await?;
        let fits = bytes.len() <= target_bytes;
        if best.as_ref().is_none_or(|b| bytes.len() < b.len()) {
            best = Some(bytes);
        }
        if fits {
            break;
        }
    }
    let bytes = best.ok_or_else(|| anyhow!("no output"))?;
    // Packs are loaded wholesale by the picker; refuse real monsters.
    if bytes.len() > target_bytes * 3 {
        bail!(
            "still {} kB after compression (limit {} kB), trim the clip",
            bytes.len() / 1000,
            target_bytes / 1000
        );
    }
    let (w, h) = webp_dims(&bytes).ok_or_else(|| anyhow!("ffmpeg produced an unreadable WebP"))?;
    tracing::debug!(src_w = src.w, src_h = src.h, frames = src.frames, w, h, size = bytes.len(), "converted");
    Ok(Converted { bytes, w, h })
}

/// VP8X animation flag.
pub fn webp_animated(b: &[u8]) -> bool {
    b.len() >= 21 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" && &b[12..16] == b"VP8X" && b[20] & 0x02 != 0
}

/// Canvas size from a WebP RIFF header (VP8X, VP8L or VP8 chunk).
pub fn webp_dims(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 30 || &b[0..4] != b"RIFF" || &b[8..12] != b"WEBP" {
        return None;
    }
    let le24 = |o: usize| u32::from(b[o]) | u32::from(b[o + 1]) << 8 | u32::from(b[o + 2]) << 16;
    match &b[12..16] {
        b"VP8X" => Some((le24(24) + 1, le24(27) + 1)),
        b"VP8L" => {
            if b[20] != 0x2f {
                return None;
            }
            let bits = u32::from_le_bytes([b[21], b[22], b[23], b[24]]);
            Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
        }
        b"VP8 " => {
            if b[23..26] != [0x9d, 0x01, 0x2a] {
                return None;
            }
            let w = u32::from(u16::from_le_bytes([b[26], b[27]]) & 0x3fff);
            let h = u32::from(u16::from_le_bytes([b[28], b[29]]) & 0x3fff);
            Some((w, h))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webp_headers() {
        // VP8X, animated, 512x384 canvas
        let mut b = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\x02\0\0\0".to_vec();
        b.extend_from_slice(&[0xff, 0x01, 0x00, 0x7f, 0x01, 0x00]);
        assert_eq!(webp_dims(&b), Some((512, 384)));
        assert!(webp_animated(&b));
        assert_eq!(webp_dims(b"RIFF0000WEBPnope"), None);
        assert_eq!(parse_rate("30000/1001").map(|f| f.round()), Some(30.0));
        assert_eq!(parse_rate("0/0"), None);
    }
}
