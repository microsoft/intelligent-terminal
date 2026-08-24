use crate::clipboard_image::PastedImage;
use std::ops::Range;

/// Per-tab attachments queued for the next user prompt.
#[derive(Default)]
pub(crate) struct PendingAttachments {
    images: Vec<PendingImage>,
    next_image_id: usize,
}

struct PendingImage {
    image: PastedImage,
    token_range: Range<usize>,
}

impl PendingAttachments {
    #[cfg(test)]
    pub fn images(&self) -> impl Iterator<Item = &PastedImage> {
        self.images.iter().map(|pending| &pending.image)
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub fn insert_image(&mut self, input: &mut String, cursor_pos: &mut usize, image: PastedImage) {
        self.next_image_id += 1;
        let token = format!(
            "[image: {}]",
            image_display_name(&image, self.next_image_id)
        );
        self.on_text_inserted(*cursor_pos, token.len());
        input.insert_str(*cursor_pos, &token);
        let token_range = *cursor_pos..*cursor_pos + token.len();
        *cursor_pos = token_range.end;
        self.images.push(PendingImage { image, token_range });
        self.images.sort_by_key(|pending| pending.token_range.start);
    }

    pub fn token_ranges(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.images
            .iter()
            .map(|pending| pending.token_range.clone())
    }

    pub fn remove_before_cursor(&mut self, input: &mut String, cursor_pos: &mut usize) -> bool {
        let Some(index) = self
            .images
            .iter()
            .position(|pending| pending.token_range.end == *cursor_pos)
        else {
            return false;
        };
        let range = self.images[index].token_range.clone();
        input.replace_range(range.clone(), "");
        self.on_text_deleted(range.clone());
        *cursor_pos = range.start;
        true
    }

    pub fn remove_at_cursor(&mut self, input: &mut String, cursor_pos: usize) -> bool {
        let Some(index) = self
            .images
            .iter()
            .position(|pending| pending.token_range.start == cursor_pos)
        else {
            return false;
        };
        let range = self.images[index].token_range.clone();
        input.replace_range(range.clone(), "");
        self.on_text_deleted(range);
        true
    }

    pub fn cursor_left(&self, cursor_pos: usize) -> Option<usize> {
        self.images
            .iter()
            .find(|pending| pending.token_range.end == cursor_pos)
            .map(|pending| pending.token_range.start)
    }

    pub fn cursor_right(&self, cursor_pos: usize) -> Option<usize> {
        self.images
            .iter()
            .find(|pending| pending.token_range.start == cursor_pos)
            .map(|pending| pending.token_range.end)
    }

    pub fn snap_cursor_left(&self, cursor_pos: usize) -> usize {
        self.images
            .iter()
            .find(|pending| pending.token_range.contains(&cursor_pos))
            .map(|pending| pending.token_range.start)
            .unwrap_or(cursor_pos)
    }

    pub fn snap_cursor_right(&self, cursor_pos: usize) -> usize {
        self.images
            .iter()
            .find(|pending| pending.token_range.contains(&cursor_pos))
            .map(|pending| pending.token_range.end)
            .unwrap_or(cursor_pos)
    }

    pub fn expand_deletion_range(&self, mut range: Range<usize>) -> Range<usize> {
        for pending in &self.images {
            if pending.token_range.start < range.end && pending.token_range.end > range.start {
                range.start = range.start.min(pending.token_range.start);
                range.end = range.end.max(pending.token_range.end);
            }
        }
        range
    }

    pub fn on_text_inserted(&mut self, position: usize, byte_len: usize) {
        for pending in &mut self.images {
            if pending.token_range.start >= position {
                pending.token_range.start += byte_len;
                pending.token_range.end += byte_len;
            }
        }
    }

    pub fn on_text_deleted(&mut self, range: Range<usize>) {
        let byte_len = range.end - range.start;
        self.images.retain(|pending| {
            pending.token_range.end <= range.start || pending.token_range.start >= range.end
        });
        for pending in &mut self.images {
            if pending.token_range.start >= range.end {
                pending.token_range.start -= byte_len;
                pending.token_range.end -= byte_len;
            }
        }
    }

    pub fn clear(&mut self) {
        self.images.clear();
    }

    pub fn remove_tokens_from_input(&mut self, input: &mut String, cursor_pos: &mut usize) {
        for pending in self.images.iter().rev() {
            let range = pending.token_range.clone();
            if *cursor_pos >= range.end {
                *cursor_pos -= range.len();
            } else if *cursor_pos > range.start {
                *cursor_pos = range.start;
            }
            input.replace_range(range, "");
        }
        self.images.clear();
    }

    pub fn take_for_submission(&mut self, mut input: String) -> (String, Vec<PastedImage>) {
        for pending in self.images.iter().rev() {
            input.replace_range(pending.token_range.clone(), "");
        }
        let images = std::mem::take(&mut self.images)
            .into_iter()
            .map(|pending| pending.image)
            .collect();
        (input, images)
    }
}

fn image_display_name(image: &PastedImage, image_id: usize) -> String {
    if std::path::Path::new(&image.label).extension().is_some() {
        return image.label.clone();
    }
    let extension = match image.mime_type.as_str() {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "png",
    };
    if image.label.eq_ignore_ascii_case("image") || image.label.eq_ignore_ascii_case("screenshot") {
        return format!("image-{image_id}.{extension}");
    }
    format!("{}.{}", image.label, extension)
}
