//! PDF form field state management

/// Form field type with associated data
#[derive(Debug, Clone)]
pub enum FormFieldType {
    /// Text input field
    Text {
        /// Current text value
        value: String,
        /// Maximum character length
        max_len: Option<i32>,
        /// Is this a multiline field
        multiline: bool,
        /// Is this a password field
        password: bool,
    },
    /// Checkbox field
    Checkbox {
        /// Current checked state
        checked: bool,
    },
    /// Radio button field
    RadioButton {
        /// Radio group name
        group: String,
        /// Current selected state
        selected: bool,
    },
    /// Dropdown/combo box field
    Dropdown {
        /// Available options
        items: Vec<String>,
        /// Currently selected item index
        selected: Option<i32>,
        /// Is the dropdown editable (combo box)
        editable: bool,
    },
    /// Signature field (display only)
    Signature,
    /// Unknown/unsupported field type
    Unknown,
}

/// Information about a form field
#[derive(Debug, Clone)]
pub struct FormFieldInfo {
    /// Unique field ID from poppler
    pub id: i32,
    /// Page number (0-indexed)
    pub page: usize,
    /// Field rectangle in PDF coordinates (x1, y1, x2, y2)
    /// Note: PDF coordinates have Y=0 at bottom
    pub rect: (f64, f64, f64, f64),
    /// Field type and current value
    pub field_type: FormFieldType,
    /// Field name (optional)
    pub name: Option<String>,
    /// Is this field read-only
    pub read_only: bool,
    /// Font size for text rendering
    pub font_size: f64,
}

/// Value that can be set on a form field
#[derive(Debug, Clone)]
pub enum FormFieldValue {
    /// Text value for text fields
    Text(String),
    /// Boolean value for checkboxes/radio buttons
    Boolean(bool),
    /// Index value for dropdown selection
    ChoiceIndex(i32),
}

/// Form state manager
#[derive(Debug, Default)]
pub struct FormState {
    /// All form fields across all pages
    pub fields: Vec<FormFieldInfo>,
    /// Currently focused field ID
    pub focused_field: Option<i32>,
    /// Page of the focused field
    pub focused_page: usize,
    /// Text buffer for text field editing
    pub text_buffer: String,
    /// Cursor position in text buffer
    pub cursor_pos: usize,
    /// Has the form been modified
    pub modified: bool,
    /// Is dropdown picker open
    pub dropdown_open: bool,
    /// Selected item in dropdown picker
    pub dropdown_selection: usize,
}

impl FormState {
    /// Create new empty form state
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if any form fields exist
    pub fn has_fields(&self) -> bool {
        !self.fields.is_empty()
    }

    /// Get fields on a specific page
    pub fn fields_on_page(&self, page: usize) -> impl Iterator<Item = &FormFieldInfo> {
        self.fields.iter().filter(move |f| f.page == page)
    }

    /// Find a field by ID
    pub fn field_by_id(&self, id: i32) -> Option<&FormFieldInfo> {
        self.fields.iter().find(|f| f.id == id)
    }

    /// Find a mutable field by ID
    pub fn field_by_id_mut(&mut self, id: i32) -> Option<&mut FormFieldInfo> {
        self.fields.iter_mut().find(|f| f.id == id)
    }

    /// Get the currently focused field
    pub fn focused(&self) -> Option<&FormFieldInfo> {
        self.focused_field.and_then(|id| self.field_by_id(id))
    }

    /// Focus a field by ID
    pub fn focus_field(&mut self, id: i32) -> bool {
        // Find field and extract needed info to avoid borrow issues
        let field_info = self.fields.iter().find(|f| f.id == id).map(|f| {
            (f.read_only, f.page, f.field_type.clone())
        });

        let Some((read_only, page, field_type)) = field_info else {
            return false;
        };

        if read_only {
            return false;
        }

        self.focused_field = Some(id);
        self.focused_page = page;

        // Initialize text buffer for text fields
        if let FormFieldType::Text { value, .. } = field_type {
            self.text_buffer = value;
            self.cursor_pos = self.text_buffer.len();
        } else {
            self.text_buffer.clear();
            self.cursor_pos = 0;
        }

        self.dropdown_open = false;
        self.dropdown_selection = 0;
        true
    }

    /// Unfocus the current field, optionally committing changes
    pub fn unfocus(&mut self, commit: bool) -> Option<(i32, FormFieldValue)> {
        let result = if commit {
            self.commit_current()
        } else {
            None
        };

        self.focused_field = None;
        self.text_buffer.clear();
        self.cursor_pos = 0;
        self.dropdown_open = false;

        result
    }

    /// Commit the current text buffer to the focused field
    pub fn commit_current(&mut self) -> Option<(i32, FormFieldValue)> {
        let id = self.focused_field?;

        // Find field index to avoid borrow issues
        let field_idx = self.fields.iter().position(|f| f.id == id)?;

        // Check if it's a text field and if value changed
        let needs_update = match &self.fields[field_idx].field_type {
            FormFieldType::Text { value, .. } => *value != self.text_buffer,
            _ => false,
        };

        if needs_update {
            // Now safe to mutate since we're not borrowing text_buffer
            let new_value = self.text_buffer.clone();
            if let FormFieldType::Text { value, .. } =
                &mut self.fields[field_idx].field_type
            {
                *value = new_value.clone();
            }
            self.modified = true;
            Some((id, FormFieldValue::Text(new_value)))
        } else {
            None
        }
    }

    /// Focus the next field in reading order (Tab)
    pub fn focus_next(&mut self) -> bool {
        let sorted = self.sorted_field_ids();
        if sorted.is_empty() {
            return false;
        }

        let next_id = if let Some(current_id) = self.focused_field {
            // Find current position and get next
            if let Some(pos) = sorted.iter().position(|&id| id == current_id) {
                let next_pos = (pos + 1) % sorted.len();
                sorted[next_pos]
            } else {
                sorted[0]
            }
        } else {
            sorted[0]
        };

        self.focus_field(next_id)
    }

    /// Focus the previous field in reading order (Shift+Tab)
    pub fn focus_prev(&mut self) -> bool {
        let sorted = self.sorted_field_ids();
        if sorted.is_empty() {
            return false;
        }

        let prev_id = if let Some(current_id) = self.focused_field {
            if let Some(pos) = sorted.iter().position(|&id| id == current_id) {
                let prev_pos = if pos == 0 { sorted.len() - 1 } else { pos - 1 };
                sorted[prev_pos]
            } else {
                sorted[0]
            }
        } else {
            sorted[sorted.len() - 1]
        };

        self.focus_field(prev_id)
    }

    /// Get field IDs sorted by reading order (page, then Y descending, then X)
    fn sorted_field_ids(&self) -> Vec<i32> {
        let mut fields: Vec<_> = self
            .fields
            .iter()
            .filter(|f| !f.read_only)
            .collect();

        // Sort by page, then by Y (descending because PDF Y=0 is at bottom),
        // then by X
        fields.sort_by(|a, b| {
            a.page
                .cmp(&b.page)
                .then_with(|| {
                    // Higher Y in PDF = higher on page = earlier in reading order
                    b.rect.1.partial_cmp(&a.rect.1).unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    a.rect.0.partial_cmp(&b.rect.0).unwrap_or(std::cmp::Ordering::Equal)
                })
        });

        fields.iter().map(|f| f.id).collect()
    }

    /// Find field at a given position (PDF coordinates)
    pub fn field_at_position(&self, page: usize, x: f64, y: f64) -> Option<&FormFieldInfo> {
        self.fields.iter().find(|f| {
            f.page == page
                && x >= f.rect.0
                && x <= f.rect.2
                && y >= f.rect.1
                && y <= f.rect.3
        })
    }

    /// Insert character at cursor position
    pub fn insert_char(&mut self, c: char) -> bool {
        if let Some(field) = self.focused() {
            if let FormFieldType::Text { max_len, .. } = &field.field_type {
                // Check max length
                if let Some(max) = max_len {
                    if self.text_buffer.len() >= *max as usize {
                        return false;
                    }
                }

                self.text_buffer.insert(self.cursor_pos, c);
                self.cursor_pos += c.len_utf8();
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Delete character before cursor
    pub fn backspace(&mut self) -> bool {
        if self.cursor_pos > 0 && !self.text_buffer.is_empty() {
            // Find the start of the previous character
            let prev_char_start = self.text_buffer[..self.cursor_pos]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);

            self.text_buffer.remove(prev_char_start);
            self.cursor_pos = prev_char_start;
            true
        } else {
            false
        }
    }

    /// Delete character after cursor
    pub fn delete(&mut self) -> bool {
        if self.cursor_pos < self.text_buffer.len() {
            self.text_buffer.remove(self.cursor_pos);
            true
        } else {
            false
        }
    }

    /// Move cursor left
    pub fn cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos = self.text_buffer[..self.cursor_pos]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    /// Move cursor right
    pub fn cursor_right(&mut self) {
        if self.cursor_pos < self.text_buffer.len() {
            self.cursor_pos = self.text_buffer[self.cursor_pos..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor_pos + i)
                .unwrap_or(self.text_buffer.len());
        }
    }

    /// Move cursor to start
    pub fn cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    /// Move cursor to end
    pub fn cursor_end(&mut self) {
        self.cursor_pos = self.text_buffer.len();
    }

    /// Toggle checkbox state
    pub fn toggle_checkbox(&mut self) -> Option<(i32, FormFieldValue)> {
        let id = self.focused_field?;

        // Find field index to avoid borrow issues
        let field_idx = self.fields.iter().position(|f| f.id == id)?;

        let result = match &mut self.fields[field_idx].field_type {
            FormFieldType::Checkbox { checked } => {
                *checked = !*checked;
                Some((id, FormFieldValue::Boolean(*checked)))
            }
            FormFieldType::RadioButton { selected, .. } => {
                *selected = true;
                Some((id, FormFieldValue::Boolean(true)))
            }
            _ => None,
        };

        if result.is_some() {
            self.modified = true;
        }

        result
    }

    /// Select dropdown item
    pub fn select_dropdown_item(&mut self, index: i32) -> Option<(i32, FormFieldValue)> {
        let id = self.focused_field?;

        // Find field index to avoid borrow issues
        let field_idx = self.fields.iter().position(|f| f.id == id)?;

        let result = match &mut self.fields[field_idx].field_type {
            FormFieldType::Dropdown { selected, items, .. } => {
                if index >= 0 && (index as usize) < items.len() {
                    *selected = Some(index);
                    Some((id, FormFieldValue::ChoiceIndex(index)))
                } else {
                    None
                }
            }
            _ => None,
        };

        if result.is_some() {
            self.modified = true;
            self.dropdown_open = false;
        }

        result
    }

    /// Toggle dropdown open state
    pub fn toggle_dropdown(&mut self) {
        if let Some(field) = self.focused() {
            if matches!(field.field_type, FormFieldType::Dropdown { .. }) {
                self.dropdown_open = !self.dropdown_open;
                self.dropdown_selection = 0;
            }
        }
    }
}
