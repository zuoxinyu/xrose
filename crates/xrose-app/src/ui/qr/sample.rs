//! Locating a QR in display coordinates and preparing its import preview.
use super::{
    decode::{self, Code},
    platform,
};
use crate::ui::tr;
use gpui_kit::SharedString;
use xrose_core::{service::SharePreview, settings::Language};

pub(super) const SNAP_DISTANCE: f32 = 82.;
const PADDING: f32 = 5.;
pub(super) type Rect = [f32; 4];

pub(super) struct ScanSummary {
    pub(super) name: Option<SharedString>,
    pub(super) detail: SharedString,
}

pub(super) fn brackets(code: &Code, dims: [f32; 2]) -> Rect {
    let [l, t, r, b] = code.rect;
    [
        l * dims[0] - PADDING,
        t * dims[1] - PADDING,
        (r - l) * dims[0] + 2. * PADDING,
        (b - t) * dims[1] + 2. * PADDING,
    ]
}

pub(super) fn nearest(codes: &[Code], pos: [f32; 2], dims: [f32; 2]) -> Option<(usize, Rect)> {
    codes
        .iter()
        .enumerate()
        .filter_map(|(i, code)| {
            let [l, t, r, b] = code.rect;
            let distance = ((l + r) / 2. * dims[0] - pos[0]).hypot((t + b) / 2. * dims[1] - pos[1]);
            let contains = (l * dims[0]..=r * dims[0]).contains(&pos[0])
                && (t * dims[1]..=b * dims[1]).contains(&pos[1]);
            (contains || distance < SNAP_DISTANCE).then_some((distance, i))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, i)| (i, brackets(&codes[i], dims)))
}

/// The preview pill confirms the previously highlighted result. A single code
/// can also attract the reticle inside its frame, but not across a blank display.
pub(super) fn target_index(
    codes: &[Code],
    pos: [f32; 2],
    dims: [f32; 2],
    side: f32,
    pill: Option<usize>,
) -> Option<usize> {
    if let Some(i) = pill.filter(|&i| i < codes.len()) {
        return Some(i);
    }
    nearest(codes, pos, dims).map(|(i, _)| i).or_else(|| {
        let [only] = codes else {
            return None;
        };
        let [l, t, r, b] = only.rect;
        (((l + r) / 2. * dims[0] - pos[0]).abs() <= side / 2.
            && ((t + b) / 2. * dims[1] - pos[1]).abs() <= side / 2.)
            .then_some(0)
    })
}
/// Normalized origin and size of a slightly larger region than the visible brackets.
pub(super) fn analysis_region(position: [f32; 2], dims: [f32; 2], finder_size: f32) -> Rect {
    let side = finder_size * 2.;
    let w = side.min(dims[0]);
    let h = side.min(dims[1]);
    [
        (position[0] - w / 2.).clamp(0., dims[0] - w) / dims[0],
        (position[1] - h / 2.).clamp(0., dims[1] - h) / dims[1],
        w / dims[0],
        h / dims[1],
    ]
}

pub(super) fn inside(pos: [f32; 2], dims: [f32; 2]) -> bool {
    (0.0..dims[0]).contains(&pos[0]) && (0.0..dims[1]).contains(&pos[1])
}

fn remap(codes: &mut [Code], region: Rect) {
    for code in codes {
        for (i, v) in code.rect.iter_mut().enumerate() {
            *v = region[i % 2] + *v * region[2 + i % 2];
        }
    }
}

pub(super) fn decode_snapshot(frame: &image::RgbaImage, region: Rect) -> Vec<Code> {
    let (w, h) = frame.dimensions();
    if w == 0 || h == 0 {
        return vec![];
    }
    let bounds = |start: f32, span: f32, size: u32| {
        let left = (start * size as f32).floor() as u32;
        let right = ((start + span) * size as f32).ceil() as u32;
        (left, right.min(size).saturating_sub(left))
    };
    let (x, cw) = bounds(region[0], region[2], w);
    let (y, ch) = bounds(region[1], region[3], h);
    if cw > 0 && ch > 0 && (cw != w || ch != h) {
        let crop = image::imageops::crop_imm(frame, x, y, cw, ch);
        let mut codes = decode::screen_codes(&*crop);
        if !codes.is_empty() {
            remap(
                &mut codes,
                [
                    x as f32 / w as f32,
                    y as f32 / h as f32,
                    cw as f32 / w as f32,
                    ch as f32 / h as f32,
                ],
            );
            return codes;
        }
    }
    decode::screen_codes(frame)
}

pub(super) async fn scan_desktop(
    target: platform::CaptureTarget,
    region: Rect,
) -> Result<Vec<Code>, platform::CaptureError> {
    let local = platform::capture(target, region).await?;
    let mut codes = decode::screen_codes(&local);
    remap(&mut codes, region);
    if codes.is_empty() && region != [0., 0., 1., 1.] {
        let overview = platform::capture(target, [0., 0., 1., 1.]).await?;
        codes = decode::screen_codes(&overview);
    }
    Ok(codes)
}

pub(super) fn scan_summary(preview: SharePreview, privacy: bool, lang: Language) -> ScanSummary {
    let (name, detail) = match preview {
        SharePreview::Node {
            mut name,
            protocol,
            endpoint,
            count,
        } => {
            let endpoint = if privacy {
                let masked = super::super::nodes::format_endpoint(&endpoint, true);
                // Unnamed links often use the server address as part of their default name.
                if let Some((host, _)) = endpoint.rsplit_once(':') {
                    let host = host.trim_matches(['[', ']']);
                    if !host.is_empty() {
                        name = name.replace(host, "••••••");
                    }
                }
                masked
            } else {
                endpoint
            };
            let mut label = format!("{protocol} · {endpoint}");
            if count > 1 {
                label.push_str(&format!(" · {count} {}", tr(lang, "个节点", "nodes")));
            }
            let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
            (
                (!name.is_empty()).then(|| name.into()),
                SharedString::from(label),
            )
        }
        SharePreview::Subscription { host } => (
            None,
            format!(
                "{} · {}",
                tr(lang, "节点订阅", "Subscription"),
                if privacy { "••••••" } else { &host }
            )
            .into(),
        ),
        SharePreview::Unsupported => (
            None,
            tr(lang, "无法识别为节点或订阅", "Not a node or subscription"),
        ),
    };
    ScanSummary { name, detail }
}

#[cfg(test)]
mod tests {
    use super::{Language, SharePreview, scan_summary};

    #[test]
    fn decoded_crop_and_large_code_fallback_select_the_same_payload() {
        use super::{decode_snapshot, target_index};
        use image::{Rgba, RgbaImage};
        let text = "socks://scan.example:1080#Demo";
        let qr = qrcode::QrCode::new(text).unwrap();
        let frame = RgbaImage::from_fn(800, 400, |x, y| {
            let module = x
                .checked_sub(500)
                .zip(y.checked_sub(80))
                .map(|(x, y)| ((x / 5) as usize, (y / 5) as usize));
            let dark = module.is_some_and(|(x, y)| {
                x < qr.width() && y < qr.width() && qr[(x, y)] == qrcode::Color::Dark
            });
            let v = if dark { 0 } else { 255 };
            Rgba([v, v, v, 255])
        });
        for region in [[0.55, 0.08, 0.4, 0.82], [0.68, 0.29, 0.06, 0.1]] {
            let codes = decode_snapshot(&frame, region);
            let i = target_index(&codes, [285., 75.], [400., 200.], 120., None).unwrap();
            assert_eq!(codes[i].text, text);
            assert!((codes[i].rect[0] - 0.625).abs() < 0.005);
            assert!(matches!(
                xrose_core::AppService::preview_shared_content(&codes[i].text),
                SharePreview::Node { .. }
            ));
        }
    }

    #[test]
    fn resizing_at_display_edges_keeps_local_sample_in_bounds() {
        use super::analysis_region;
        assert_eq!(
            analysis_region([0., 900.], [1600., 900.], 120.),
            [0., 660. / 900., 240. / 1600., 240. / 900.]
        );
        assert_eq!(
            analysis_region([1599., 0.], [1600., 900.], 800.),
            [0., 0., 1., 1.]
        );
        assert!(!super::inside([-1., 30.], [1600., 900.]));
        assert!(!super::inside([1600., 30.], [1600., 900.]));
    }

    #[test]
    fn confirm_uses_only_the_highlighted_qr_or_its_preview() {
        use super::{Code, target_index};
        let codes = [
            Code {
                rect: [0.1, 0.1, 0.2, 0.2],
                text: "first".into(),
            },
            Code {
                rect: [0.7, 0.7, 0.8, 0.8],
                text: "second".into(),
            },
        ];
        let dims = [1000., 1000.];
        let pick = |pos, last| target_index(&codes, pos, dims, 280., last);
        assert_eq!(pick([700., 710.], None), Some(1));
        assert_eq!(pick([500., 900.], Some(1)), Some(1));
        assert_eq!(pick([500., 900.], Some(3)), None);
        assert_eq!(pick([500., 500.], None), None);
        assert_eq!(
            target_index(&codes[..1], [270., 150.], dims, 280., None),
            Some(0)
        );
        assert_eq!(
            target_index(&codes[..1], [500., 500.], dims, 280., None),
            None
        );
    }

    #[test]
    fn node_summary_respects_endpoint_privacy_in_default_names() {
        let preview = |name: &str| SharePreview::Node {
            name: name.into(),
            protocol: "SOCKS",
            endpoint: "node.example:1080".into(),
            count: 1,
        };
        let label = scan_summary(preview("测试\n节点"), false, Language::Zh);
        assert_eq!(label.name.as_deref(), Some("测试 节点"));
        assert_eq!(label.detail.as_ref(), "SOCKS · node.example:1080");

        let label = scan_summary(preview("SOCKS node.example"), true, Language::En);
        assert_eq!(label.name.as_deref(), Some("SOCKS ••••••"));
        assert_eq!(label.detail.as_ref(), "SOCKS · ••••••:1080");
    }
}
