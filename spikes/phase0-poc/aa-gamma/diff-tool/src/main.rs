//! aa-diff — aligned pixel diff for the AA/gamma spike.
//!
//! Takes two PNGs (the SwiftTerm fixture capture as --ref, the GPUI capture
//! as --img), aligns them by translation (the two apps' cell origins/padding
//! differ by a few px) via coarse-to-fine cross-correlation of gradient maps,
//! then reports:
//!   * per-channel max delta and mean |delta|
//!   * RMSE per channel
//!   * % of pixels differing beyond --threshold (any channel)
//!   * a per-scene-row breakdown (--cell-px device pixels per row, rows
//!     counted from the REF image's origin — the fixture drawable IS the
//!     grid, so its (0,0) is the grid origin)
//! and writes:
//!   * {out}-heatmap.png   amplified |delta| (x8, max across channels)
//!   * {out}-ref.png / {out}-img.png  the aligned intersection crops
//!   * {out}-report.json   machine-readable metrics
//!
//! Alignment is integer-pixel. Sub-pixel placement differences between the
//! two renderers (GPUI floor-snap + baked subpixel variants vs SwiftTerm
//! fractional bilinear placement) are part of the MEASUREMENT, not noise —
//! the tool therefore also reports the best-shift correlation score so a
//! poor lock is visible.

use std::path::PathBuf;

struct Img {
    w: usize,
    h: usize,
    rgba: Vec<u8>,
}

fn load_png(path: &PathBuf) -> Img {
    let f = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut reader = png::Decoder::new(std::io::BufReader::new(f))
        .read_info()
        .unwrap_or_else(|e| panic!("decode {path:?}: {e}"));
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png frame");
    let w = info.width as usize;
    let h = info.height as usize;
    assert_eq!(info.bit_depth, png::BitDepth::Eight, "expect 8-bit png");
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..w * h * 4].to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(w * h * 4);
            for px in buf[..w * h * 3].chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        other => panic!("unsupported png color type {other:?}"),
    };
    Img { w, h, rgba }
}

fn to_gray(img: &Img) -> Vec<f32> {
    img.rgba
        .chunks_exact(4)
        .map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32)
        .collect()
}

/// |dx| + |dy| gradient magnitude — alignment features independent of theme
/// polarity and absolute levels.
fn gradient(gray: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut g = vec![0.0f32; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let dx = gray[i + 1] - gray[i - 1];
            let dy = gray[i + w] - gray[i - w];
            g[i] = dx.abs() + dy.abs();
        }
    }
    g
}

fn downsample(src: &[f32], w: usize, h: usize, f: usize) -> (Vec<f32>, usize, usize) {
    let dw = w / f;
    let dh = h / f;
    let mut out = vec![0.0f32; dw * dh];
    for y in 0..dh {
        for x in 0..dw {
            let mut s = 0.0;
            for yy in 0..f {
                for xx in 0..f {
                    s += src[(y * f + yy) * w + (x * f + xx)];
                }
            }
            out[y * dw + x] = s;
        }
    }
    (out, dw, dh)
}

/// Normalized correlation of a's and b's overlap when b is shifted by
/// (sx, sy) relative to a. Returns None when the overlap is too small.
fn corr(
    a: &[f32],
    aw: usize,
    ah: usize,
    b: &[f32],
    bw: usize,
    bh: usize,
    sx: isize,
    sy: isize,
) -> Option<f64> {
    let x0 = 0.max(sx) as usize;
    let y0 = 0.max(sy) as usize;
    let x1 = (aw as isize).min(bw as isize + sx) as usize;
    let y1 = (ah as isize).min(bh as isize + sy) as usize;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (ow, oh) = (x1 - x0, y1 - y0);
    if ow * oh < (aw * ah) / 4 {
        return None; // demand >= 25% overlap
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for y in y0..y1 {
        let by = (y as isize - sy) as usize;
        for x in x0..x1 {
            let bx = (x as isize - sx) as usize;
            let va = a[y * aw + x] as f64;
            let vb = b[by * bw + bx] as f64;
            dot += va * vb;
            na += va * va;
            nb += vb * vb;
        }
    }
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some(dot / (na.sqrt() * nb.sqrt()))
}

fn best_shift(
    a: &[f32],
    aw: usize,
    ah: usize,
    b: &[f32],
    bw: usize,
    bh: usize,
    range: isize,
    center: (isize, isize),
) -> ((isize, isize), f64) {
    let mut best = ((0, 0), -1.0f64);
    for sy in (center.1 - range)..=(center.1 + range) {
        for sx in (center.0 - range)..=(center.0 + range) {
            if let Some(c) = corr(a, aw, ah, b, bw, bh, sx, sy) {
                if c > best.1 {
                    best = ((sx, sy), c);
                }
            }
        }
    }
    best
}

struct Args {
    reference: PathBuf,
    img: PathBuf,
    out: PathBuf, // prefix
    threshold: u8,
    cell_px: usize,
    rows: usize,
    max_shift: isize,
}

fn parse_args() -> Args {
    let mut a = Args {
        reference: PathBuf::new(),
        img: PathBuf::new(),
        out: PathBuf::new(),
        threshold: 8,
        cell_px: 32,
        rows: 16,
        max_shift: 96,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| panic!("missing value for {k}"));
        match k.as_str() {
            "--ref" => a.reference = PathBuf::from(val()),
            "--img" => a.img = PathBuf::from(val()),
            "--out" => a.out = PathBuf::from(val()),
            "--threshold" => a.threshold = val().parse().unwrap(),
            "--cell-px" => a.cell_px = val().parse().unwrap(),
            "--rows" => a.rows = val().parse().unwrap(),
            "--max-shift" => a.max_shift = val().parse().unwrap(),
            other => panic!("unknown arg: {other}"),
        }
    }
    assert!(
        !a.reference.as_os_str().is_empty()
            && !a.img.as_os_str().is_empty()
            && !a.out.as_os_str().is_empty(),
        "usage: aa-diff --ref fixture.png --img gpui.png --out out/prefix [--threshold 8] [--cell-px 32] [--rows 16]"
    );
    a
}

fn write_png(path: &PathBuf, w: usize, h: usize, rgba: &[u8]) {
    let f = std::fs::File::create(path).unwrap_or_else(|e| panic!("create {path:?}: {e}"));
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .expect("png header")
        .write_image_data(rgba)
        .expect("png data");
}

fn main() {
    let args = parse_args();
    let a = load_png(&args.reference);
    let b = load_png(&args.img);

    eprintln!(
        "[aa-diff] ref {:?} {}x{} | img {:?} {}x{}",
        args.reference, a.w, a.h, args.img, b.w, b.h
    );

    // ---- alignment ----
    let ga = gradient(&to_gray(&a), a.w, a.h);
    let gb = gradient(&to_gray(&b), b.w, b.h);
    const F: usize = 4;
    let (da, daw, dah) = downsample(&ga, a.w, a.h, F);
    let (db, dbw, dbh) = downsample(&gb, b.w, b.h, F);
    let (coarse, _c1) = best_shift(&da, daw, dah, &db, dbw, dbh, args.max_shift / F as isize, (0, 0));
    let ((sx, sy), score) = best_shift(
        &ga,
        a.w,
        a.h,
        &gb,
        b.w,
        b.h,
        F as isize + 3,
        (coarse.0 * F as isize, coarse.1 * F as isize),
    );
    eprintln!(
        "[aa-diff] alignment: img shifted by ({sx}, {sy}) px relative to ref; correlation {score:.4}"
    );
    if score < 0.5 {
        eprintln!(
            "[aa-diff] WARNING: weak alignment lock (correlation {score:.3} < 0.5) — treat metrics with suspicion"
        );
    }

    // ---- intersection in ref coords ----
    let x0 = 0.max(sx) as usize;
    let y0 = 0.max(sy) as usize;
    let x1 = (a.w as isize).min(b.w as isize + sx) as usize;
    let y1 = (a.h as isize).min(b.h as isize + sy) as usize;
    assert!(x1 > x0 && y1 > y0, "no overlap after alignment");
    let (ow, oh) = (x1 - x0, y1 - y0);
    eprintln!("[aa-diff] intersection {ow}x{oh} at ref({x0},{y0})");

    // ---- metrics + heatmap ----
    let mut max_d = [0u8; 3];
    let mut sum_d = [0f64; 3];
    let mut sum_sq = [0f64; 3];
    let mut over = 0usize;
    let mut per_row: Vec<(usize, [f64; 2], usize, usize)> = Vec::new(); // (row, [mean, max], over, count)
    let mut row_acc: Vec<(f64, f64, usize, usize)> = vec![(0.0, 0.0, 0, 0); args.rows + 1];

    let mut heat = vec![0u8; ow * oh * 4];
    let mut ref_crop = vec![0u8; ow * oh * 4];
    let mut img_crop = vec![0u8; ow * oh * 4];

    for y in y0..y1 {
        let by = (y as isize - sy) as usize;
        let row_idx = (y / args.cell_px).min(args.rows); // rows beyond grid -> bucket args.rows
        for x in x0..x1 {
            let bx = (x as isize - sx) as usize;
            let ia = (y * a.w + x) * 4;
            let ib = (by * b.w + bx) * 4;
            let mut worst = 0u8;
            for c in 0..3 {
                let d = a.rgba[ia + c].abs_diff(b.rgba[ib + c]);
                if d > max_d[c] {
                    max_d[c] = d;
                }
                sum_d[c] += d as f64;
                sum_sq[c] += (d as f64) * (d as f64);
                if d > worst {
                    worst = d;
                }
            }
            if worst > args.threshold {
                over += 1;
            }
            let acc = &mut row_acc[row_idx];
            acc.0 += worst as f64;
            if (worst as f64) > acc.1 {
                acc.1 = worst as f64;
            }
            if worst > args.threshold {
                acc.2 += 1;
            }
            acc.3 += 1;

            let o = ((y - y0) * ow + (x - x0)) * 4;
            let amp = (worst as u32 * 8).min(255) as u8;
            heat[o] = amp;
            heat[o + 1] = if worst > args.threshold { 0 } else { amp };
            heat[o + 2] = if worst > args.threshold { 0 } else { amp };
            heat[o + 3] = 255;
            ref_crop[o..o + 4].copy_from_slice(&a.rgba[ia..ia + 4]);
            img_crop[o..o + 4].copy_from_slice(&b.rgba[ib..ib + 4]);
        }
    }
    let n = (ow * oh) as f64;
    for (r, acc) in row_acc.iter().enumerate() {
        if acc.3 > 0 {
            per_row.push((r, [acc.0 / acc.3 as f64, acc.1], acc.2, acc.3));
        }
    }

    let mean = [sum_d[0] / n, sum_d[1] / n, sum_d[2] / n];
    let rmse = [
        (sum_sq[0] / n).sqrt(),
        (sum_sq[1] / n).sqrt(),
        (sum_sq[2] / n).sqrt(),
    ];
    let pct_over = 100.0 * over as f64 / n;

    println!("== aa-diff report ==");
    println!("ref: {:?}", args.reference);
    println!("img: {:?}", args.img);
    println!("shift (img rel ref): ({sx}, {sy}) px, correlation {score:.4}");
    println!("intersection: {ow}x{oh}");
    println!(
        "max delta   R {} G {} B {}   (0-255)",
        max_d[0], max_d[1], max_d[2]
    );
    println!(
        "mean |delta| R {:.3} G {:.3} B {:.3}",
        mean[0], mean[1], mean[2]
    );
    println!(
        "RMSE        R {:.3} G {:.3} B {:.3}",
        rmse[0], rmse[1], rmse[2]
    );
    println!(
        "pixels with any channel > {}: {} / {} = {:.3}%",
        args.threshold,
        over,
        ow * oh,
        pct_over
    );
    println!("per-scene-row (ref y / {} px):", args.cell_px);
    println!("  row | mean|d| | max|d| | %>thr  (rows 13=bold, 14=underline are non-curve axes)");
    for (r, m, o, cnt) in &per_row {
        println!(
            "  {:>3} | {:>7.3} | {:>6.0} | {:>6.3}%",
            if *r == args.rows {
                "pad".to_string()
            } else {
                r.to_string()
            },
            m[0],
            m[1],
            100.0 * *o as f64 / *cnt as f64
        );
    }

    // ---- outputs ----
    if let Some(dir) = args.out.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).expect("create out dir");
        }
    }
    let p = |suffix: &str| -> PathBuf {
        let mut s = args.out.as_os_str().to_owned();
        s.push(suffix);
        PathBuf::from(s)
    };
    write_png(&p("-heatmap.png"), ow, oh, &heat);
    write_png(&p("-ref.png"), ow, oh, &ref_crop);
    write_png(&p("-img.png"), ow, oh, &img_crop);

    let mut rows_json = String::new();
    for (r, m, o, cnt) in &per_row {
        rows_json.push_str(&format!(
            "    {{\"row\": {}, \"mean\": {:.4}, \"max\": {:.0}, \"pct_over\": {:.4}}},\n",
            r,
            m[0],
            m[1],
            100.0 * *o as f64 / *cnt as f64
        ));
    }
    let rows_json = rows_json.trim_end_matches(",\n").to_string();
    let report = format!(
        "{{\n  \"ref\": {:?},\n  \"img\": {:?},\n  \"shift\": [{sx}, {sy}],\n  \"correlation\": {score:.6},\n  \"intersection\": [{ow}, {oh}],\n  \"max_delta\": [{}, {}, {}],\n  \"mean_delta\": [{:.4}, {:.4}, {:.4}],\n  \"rmse\": [{:.4}, {:.4}, {:.4}],\n  \"threshold\": {},\n  \"pct_over_threshold\": {:.4},\n  \"per_row\": [\n{}\n  ]\n}}\n",
        args.reference,
        args.img,
        max_d[0],
        max_d[1],
        max_d[2],
        mean[0],
        mean[1],
        mean[2],
        rmse[0],
        rmse[1],
        rmse[2],
        args.threshold,
        pct_over,
        rows_json
    );
    std::fs::write(p("-report.json"), report).expect("write report");
    eprintln!(
        "[aa-diff] wrote {}-heatmap.png / -ref.png / -img.png / -report.json",
        args.out.display()
    );
}
