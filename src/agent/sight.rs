//! Reading the screen without sending it anywhere.
//!
//! A screenshot is the most expensive thing an agent can look at: a full desktop is a couple of
//! thousand image tokens at the far end, and the answer it wants is usually "where is the OK
//! button". Two small ONNX models answer that here instead, on the CPU, in tens of
//! milliseconds:
//!
//! * **text** -- PP-OCRv6 tiny, a 1.7 MB DBNet detector and a 4.3 MB CTC recogniser. Detection
//!   is a threshold and connected components over a probability map; recognition is a greedy
//!   decode over per-column class scores. Both are a page of code, which is why this talks to
//!   ONNX Runtime rather than through an OCR toolkit.
//! * **targets** -- an 11.7 MB YOLO trained on interface elements, which finds the things worth
//!   clicking whether or not they carry a label. Icons have no text for the OCR to read.
//!
//! Neither model is bundled: [`Models::load`] looks in the models directory and the tools are
//! only offered when it finds them. Nothing here reaches the network, at any point.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use image::RgbImage;
use ort::session::Session;
use ort::value::Tensor;

/// The file names looked for in the models directory.
const TEXT_DETECTOR: &str = "text-det.onnx";
const TEXT_RECOGNISER: &str = "text-rec.onnx";
const CHARSET: &str = "charset.txt";
const WIDGET_DETECTOR: &str = "widgets.onnx";

/// The longest side DBNet sees. Detection scales with this and the model was trained around it:
/// asking for more finds *fewer* boxes and takes twenty times as long, because screen text then
/// sits at a size the network was never shown.
const DETECT_SIDE: u32 = 960;

/// The height every line is scaled to before recognition, which is what the model expects.
const LINE_HEIGHT: u32 = 48;

/// YOLO's fixed input. A 4K desktop and a small dialog cost the same because of it.
const WIDGET_SIDE: u32 = 640;

/// One line of text, and where it is in desktop pixels.
#[derive(Debug, Clone)]
pub struct Line {
    pub text: String,
    pub confidence: f32,
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Line {
    /// The middle, which is where a click on this line goes.
    pub fn centre(&self) -> (u16, u16) {
        (
            ((self.left + self.right) / 2) as u16,
            ((self.top + self.bottom) / 2) as u16,
        )
    }
}

/// Something that looks clickable, with no claim about what it is.
#[derive(Debug, Clone)]
pub struct Target {
    pub confidence: f32,
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Target {
    pub fn centre(&self) -> (u16, u16) {
        (
            ((self.left + self.right) / 2) as u16,
            ((self.top + self.bottom) / 2) as u16,
        )
    }
}

/// The models, loaded once and used from whichever thread asks.
///
/// ONNX Runtime sessions are not `Sync` in a way that allows concurrent `run`, and a screenshot
/// is not worth the complexity of a pool: one at a time, and each look is milliseconds.
pub struct Models {
    text_detector: Mutex<Session>,
    text_recogniser: Mutex<Session>,
    widget_detector: Mutex<Session>,
    alphabet: Vec<String>,
}

impl std::fmt::Debug for Models {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Models")
            .field("alphabet", &self.alphabet.len())
            .finish_non_exhaustive()
    }
}

impl Models {
    /// Where the models live unless the preferences say otherwise.
    pub fn default_directory() -> Option<PathBuf> {
        dirs::data_dir().map(|dir| dir.join("irontsc/models"))
    }

    /// Whether all model files are present, without starting ONNX Runtime or loading them.
    pub fn available(directory: &Path) -> bool {
        [TEXT_DETECTOR, TEXT_RECOGNISER, CHARSET, WIDGET_DETECTOR]
            .iter()
            .all(|name| directory.join(name).is_file())
    }

    /// Loads what is there, or `None` when the directory has no models in it.
    ///
    /// Missing models are not an error: they are the ordinary case for someone who only wants a
    /// remote desktop, and the tools that need them simply are not offered.
    pub fn load(directory: &Path) -> Option<Self> {
        if !Self::available(directory) {
            return None;
        }

        match Self::open(directory) {
            Ok(models) => Some(models),
            Err(error) => {
                tracing::warn!(%error, directory = %directory.display(), "could not load the screen models");
                None
            }
        }
    }

    fn open(directory: &Path) -> anyhow::Result<Self> {
        let session = |name: &str| -> anyhow::Result<Session> {
            Ok(Session::builder()?.commit_from_file(directory.join(name))?)
        };

        // The blank class comes first and a space is appended last, which is how PaddleOCR's CTC
        // decoder is indexed; the file itself holds neither.
        let charset = std::fs::read_to_string(directory.join(CHARSET))?;
        let mut alphabet = vec!["".to_owned()];
        alphabet.extend(charset.lines().map(str::to_owned));
        alphabet.push(" ".to_owned());

        Ok(Self {
            text_detector: Mutex::new(session(TEXT_DETECTOR)?),
            text_recogniser: Mutex::new(session(TEXT_RECOGNISER)?),
            widget_detector: Mutex::new(session(WIDGET_DETECTOR)?),
            alphabet,
        })
    }

    /// Every line of text on the image, with where it sits.
    pub fn read(&self, image: &RgbImage) -> anyhow::Result<Vec<Line>> {
        let boxes = self.detect_text(image)?;
        if boxes.is_empty() {
            return Ok(Vec::new());
        }

        let crops: Vec<RgbImage> = boxes
            .iter()
            .map(|(left, top, right, bottom)| {
                image::imageops::crop_imm(image, *left, *top, right - left, bottom - top)
                    .to_image()
            })
            .collect();

        let recognised = self.recognise(&crops)?;
        Ok(boxes
            .into_iter()
            .zip(recognised)
            .filter(|(_, (text, _))| !text.trim().is_empty())
            .map(|((left, top, right, bottom), (text, confidence))| Line {
                text,
                confidence,
                left,
                top,
                right,
                bottom,
            })
            .collect())
    }

    /// DBNet: a probability map, thresholded, and one box per connected blob.
    fn detect_text(&self, image: &RgbImage) -> anyhow::Result<Vec<(u32, u32, u32, u32)>> {
        let (width, height) = image.dimensions();
        let scale = f64::from(DETECT_SIDE) / f64::from(width.max(height));
        let scale = scale.min(1.0);
        // The network wants a multiple of 32 in both directions.
        let input_width = (((f64::from(width) * scale / 32.0).round() as u32) * 32).max(32);
        let input_height = (((f64::from(height) * scale / 32.0).round() as u32) * 32).max(32);
        let resized = image::imageops::resize(
            image,
            input_width,
            input_height,
            image::imageops::FilterType::Triangle,
        );

        // ImageNet normalisation, the way the model was trained.
        const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
        const DEVIATION: [f32; 3] = [0.229, 0.224, 0.225];
        let mut input =
            vec![0f32; 3 * input_width as usize * input_height as usize];
        let plane = input_width as usize * input_height as usize;
        for (index, pixel) in resized.pixels().enumerate() {
            for channel in 0..3 {
                input[channel * plane + index] =
                    (f32::from(pixel[channel]) / 255.0 - MEAN[channel]) / DEVIATION[channel];
            }
        }

        let tensor = Tensor::from_array((
            [1, 3, input_height as usize, input_width as usize],
            input,
        ))?;
        let mut session = self
            .text_detector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let outputs = session.run(ort::inputs!["x" => tensor])?;
        let (shape, probability) = outputs[0].try_extract_tensor::<f32>()?;
        let map_height = shape[shape.len() - 2] as u32;
        let map_width = shape[shape.len() - 1] as u32;

        // A blob is text; its bounding box, padded a little, is the line.
        let mut mask = image::GrayImage::new(map_width, map_height);
        for y in 0..map_height {
            for x in 0..map_width {
                let value = probability[(y * map_width + x) as usize];
                mask.put_pixel(x, y, image::Luma([if value > 0.2 { 255 } else { 0 }]));
            }
        }

        let components = imageproc::region_labelling::connected_components(
            &mask,
            imageproc::region_labelling::Connectivity::Eight,
            image::Luma([0]),
        );

        let mut extents: std::collections::HashMap<u32, (u32, u32, u32, u32, f32, u32)> =
            std::collections::HashMap::new();
        for (x, y, label) in components.enumerate_pixels() {
            let label = label[0];
            if label == 0 {
                continue;
            }
            let value = probability[(y * map_width + x) as usize];
            let entry = extents
                .entry(label)
                .or_insert((x, y, x, y, 0.0, 0));
            entry.0 = entry.0.min(x);
            entry.1 = entry.1.min(y);
            entry.2 = entry.2.max(x);
            entry.3 = entry.3.max(y);
            entry.4 += value;
            entry.5 += 1;
        }

        let horizontal = f64::from(width) / f64::from(map_width);
        let vertical = f64::from(height) / f64::from(map_height);
        let mut boxes: Vec<(u32, u32, u32, u32)> = extents
            .into_values()
            .filter(|(left, top, right, bottom, total, count)| {
                right - left >= 3 && bottom - top >= 3 && total / *count as f32 > 0.4
            })
            .map(|(left, top, right, bottom, _, _)| {
                // Unclip: DBNet shrinks the region it was trained on, so the box is grown back.
                let pad = ((right - left).min(bottom - top) / 5).max(1);
                (
                    ((left.saturating_sub(pad)) as f64 * horizontal) as u32,
                    ((top.saturating_sub(pad)) as f64 * vertical) as u32,
                    (((right + pad + 1) as f64 * horizontal) as u32).min(width),
                    (((bottom + pad + 1) as f64 * vertical) as u32).min(height),
                )
            })
            .filter(|(left, top, right, bottom)| *right > left + 6 && *bottom > top + 6)
            .collect();

        // Reading order: down the screen, then across.
        boxes.sort_by_key(|(left, top, _, _)| (*top, *left));
        Ok(boxes)
    }

    /// Greedy CTC over a batch of line crops.
    fn recognise(&self, crops: &[RgbImage]) -> anyhow::Result<Vec<(String, f32)>> {
        let batch = crops.len();
        let width = crops
            .iter()
            .map(|crop| {
                (f64::from(crop.width()) * f64::from(LINE_HEIGHT) / f64::from(crop.height().max(1)))
                    .round() as u32
            })
            .max()
            .unwrap_or(LINE_HEIGHT)
            .clamp(LINE_HEIGHT, 3200);

        let plane = LINE_HEIGHT as usize * width as usize;
        let mut input = vec![0f32; batch * 3 * plane];
        for (index, crop) in crops.iter().enumerate() {
            let scaled_width = ((f64::from(crop.width()) * f64::from(LINE_HEIGHT)
                / f64::from(crop.height().max(1)))
            .round() as u32)
                .clamp(1, width);
            let resized = image::imageops::resize(
                crop,
                scaled_width,
                LINE_HEIGHT,
                image::imageops::FilterType::Triangle,
            );
            for (x, y, pixel) in resized.enumerate_pixels() {
                for channel in 0..3 {
                    let at = index * 3 * plane
                        + channel * plane
                        + (y * width + x) as usize;
                    // Scaled to [-1, 1], which is what the recogniser was trained on.
                    input[at] = f32::from(pixel[channel]) / 127.5 - 1.0;
                }
            }
        }

        let tensor = Tensor::from_array((
            [batch, 3, LINE_HEIGHT as usize, width as usize],
            input,
        ))?;
        let mut session = self
            .text_recogniser
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let outputs = session.run(ort::inputs!["x" => tensor])?;
        let (shape, logits) = outputs[0].try_extract_tensor::<f32>()?;
        let steps = shape[1] as usize;
        let classes = shape[2] as usize;

        Ok((0..batch)
            .map(|item| {
                let mut text = String::new();
                let mut total = 0.0f32;
                let mut counted = 0u32;
                let mut previous = usize::MAX;
                for step in 0..steps {
                    let row = &logits[(item * steps + step) * classes..][..classes];
                    let (best, score) = row.iter().enumerate().fold(
                        (0usize, f32::MIN),
                        |(best, high), (class, value)| {
                            if *value > high {
                                (class, *value)
                            } else {
                                (best, high)
                            }
                        },
                    );
                    // Blank is class zero, and a repeat of the last class is the same character.
                    if best != 0 && best != previous {
                        if let Some(character) = self.alphabet.get(best) {
                            text.push_str(character);
                        }
                        total += score;
                        counted += 1;
                    }
                    previous = best;
                }
                let confidence = if counted == 0 { 0.0 } else { total / counted as f32 };
                (text, confidence)
            })
            .collect())
    }

    /// Everything that looks clickable, most confident first.
    pub fn targets(&self, image: &RgbImage, minimum: f32) -> anyhow::Result<Vec<Target>> {
        let (width, height) = image.dimensions();
        let resized = image::imageops::resize(
            image,
            WIDGET_SIDE,
            WIDGET_SIDE,
            image::imageops::FilterType::Triangle,
        );

        let plane = (WIDGET_SIDE * WIDGET_SIDE) as usize;
        let mut input = vec![0f32; 3 * plane];
        for (index, pixel) in resized.pixels().enumerate() {
            for channel in 0..3 {
                input[channel * plane + index] = f32::from(pixel[channel]) / 255.0;
            }
        }

        let tensor = Tensor::from_array((
            [1, 3, WIDGET_SIDE as usize, WIDGET_SIDE as usize],
            input,
        ))?;
        let mut session = self
            .widget_detector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let outputs = session.run(ort::inputs!["images" => tensor])?;
        let (shape, predictions) = outputs[0].try_extract_tensor::<f32>()?;
        // (1, 5, N): centre x, centre y, width, height, confidence -- one class, so no argmax.
        let count = shape[shape.len() - 1] as usize;

        let horizontal = f64::from(width) / f64::from(WIDGET_SIDE);
        let vertical = f64::from(height) / f64::from(WIDGET_SIDE);
        let mut found: Vec<Target> = (0..count)
            .filter(|index| predictions[4 * count + index] >= minimum)
            .map(|index| {
                let centre_x = f64::from(predictions[index]);
                let centre_y = f64::from(predictions[count + index]);
                let box_width = f64::from(predictions[2 * count + index]);
                let box_height = f64::from(predictions[3 * count + index]);
                Target {
                    confidence: predictions[4 * count + index],
                    left: (((centre_x - box_width / 2.0) * horizontal).max(0.0)) as u32,
                    top: (((centre_y - box_height / 2.0) * vertical).max(0.0)) as u32,
                    right: (((centre_x + box_width / 2.0) * horizontal).min(f64::from(width)))
                        as u32,
                    bottom: (((centre_y + box_height / 2.0) * vertical).min(f64::from(height)))
                        as u32,
                }
            })
            .collect();

        found.sort_by(|left, right| {
            right
                .confidence
                .partial_cmp(&left.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(suppress_overlaps(found, 0.45))
    }
}

/// Joins the lines of one heading back together.
///
/// The detector finds lines, not paragraphs, so a headline set over three lines arrives as
/// three boxes -- and sorted down the page they interleave with whatever sits beside them in
/// the next column. Lines that share a horizontal span and follow each other closely are one
/// piece of text, and reading them as one is what makes a page of headlines legible.
pub fn into_blocks(mut lines: Vec<Line>) -> Vec<Line> {
    lines.sort_by_key(|line| (line.top, line.left));

    let mut blocks: Vec<Line> = Vec::new();
    for line in lines {
        let height = line.bottom.saturating_sub(line.top).max(1);
        let joined = blocks.iter_mut().find(|block| {
            // Directly below, within a line's height of the one above it.
            let gap = line.top.saturating_sub(block.bottom);
            if line.top < block.bottom || gap > height {
                return false;
            }
            // And sharing most of its width with what is above.
            let overlap = block.right.min(line.right).saturating_sub(block.left.max(line.left));
            let narrower = (block.right - block.left).min(line.right - line.left).max(1);
            overlap * 2 > narrower
        });

        match joined {
            Some(block) => {
                block.text.push(' ');
                block.text.push_str(&line.text);
                block.left = block.left.min(line.left);
                block.right = block.right.max(line.right);
                block.bottom = line.bottom;
                block.confidence = block.confidence.min(line.confidence);
            }
            None => blocks.push(line),
        }
    }
    blocks
}

/// Non-maximum suppression: the same button is found several times, and one box is enough.
fn suppress_overlaps(found: Vec<Target>, threshold: f32) -> Vec<Target> {
    let mut kept: Vec<Target> = Vec::new();
    for candidate in found {
        let overlaps = kept.iter().any(|existing| {
            let left = candidate.left.max(existing.left);
            let top = candidate.top.max(existing.top);
            let right = candidate.right.min(existing.right);
            let bottom = candidate.bottom.min(existing.bottom);
            if right <= left || bottom <= top {
                return false;
            }
            let overlap = f64::from(right - left) * f64::from(bottom - top);
            let candidate_area = f64::from(candidate.right - candidate.left)
                * f64::from(candidate.bottom - candidate.top);
            let existing_area = f64::from(existing.right - existing.left)
                * f64::from(existing.bottom - existing.top);
            overlap / (candidate_area + existing_area - overlap) > f64::from(threshold)
        });
        if !overlaps {
            kept.push(candidate);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_without_models_loads_nothing() {
        let empty = std::env::temp_dir().join(format!("irontsc-sight-{}", std::process::id()));
        std::fs::create_dir_all(&empty).expect("a directory");
        assert!(!Models::available(&empty));
        assert!(Models::load(&empty).is_none());
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn availability_only_checks_the_four_files() {
        let directory =
            std::env::temp_dir().join(format!("irontsc-sight-files-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("a directory");
        for name in [TEXT_DETECTOR, TEXT_RECOGNISER, CHARSET, WIDGET_DETECTOR] {
            std::fs::File::create(directory.join(name)).expect("a model placeholder");
        }
        assert!(Models::available(&directory));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn overlapping_targets_collapse_to_one() {
        let target = |left, top, right, bottom, confidence| Target {
            confidence,
            left,
            top,
            right,
            bottom,
        };
        let kept = suppress_overlaps(
            vec![
                target(10, 10, 110, 60, 0.9),
                target(12, 12, 112, 62, 0.8),  // the same button
                target(400, 300, 500, 340, 0.7), // a different one
            ],
            0.45,
        );
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].confidence, 0.9);
    }

    #[test]
    fn a_heading_over_three_lines_reads_as_one() {
        let line = |text: &str, left, top, right, bottom| Line {
            text: text.to_owned(),
            confidence: 0.9,
            left,
            top,
            right,
            bottom,
        };
        // A headline in the left column, and something else beside it in the right.
        let blocks = into_blocks(vec![
            line("Fem", 64, 462, 172, 499),
            line("Politi-advarsel.", 804, 427, 940, 443),
            line("strake tap", 66, 508, 306, 545),
            line("for meg", 66, 554, 380, 591),
        ]);

        assert_eq!(blocks.len(), 2, "two columns, two blocks");
        let headline = blocks
            .iter()
            .find(|block| block.left < 500)
            .expect("the left column");
        assert_eq!(headline.text, "Fem strake tap for meg");
        assert_eq!((headline.top, headline.bottom), (462, 591));
    }

    #[test]
    fn a_line_is_clicked_in_the_middle() {
        let line = Line {
            text: "Accept".to_owned(),
            confidence: 0.9,
            left: 804,
            top: 635,
            right: 930,
            bottom: 676,
        };
        assert_eq!(line.centre(), (867, 655));
    }
}
