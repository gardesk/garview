use super::{Backend, LinkDestination, PageSize, RenderedPage};
use crate::forms::{FormFieldInfo, FormFieldType, FormFieldValue};
use anyhow::{anyhow, Result};
use cairo::glib::translate::ToGlibPtr;
use poppler::{ffi, Document, IndexIter, Rectangle, SelectionStyle};
use std::ffi::CStr;
use std::path::{Path, PathBuf};

pub struct PdfBackend {
    document: Option<Document>,
    path: Option<PathBuf>,
}

impl PdfBackend {
    pub fn new() -> Self {
        Self {
            document: None,
            path: None,
        }
    }

    fn get_page(&self, index: usize) -> Result<poppler::Page> {
        let doc = self
            .document
            .as_ref()
            .ok_or_else(|| anyhow!("No document loaded"))?;
        doc.page(index as i32)
            .ok_or_else(|| anyhow!("Invalid page index: {}", index))
    }

    /// Extract TOC entries recursively from an IndexIter
    fn extract_toc_entries(
        doc: &Document,
        iter: &mut IndexIter,
        level: usize,
        entries: &mut Vec<(String, usize, usize)>,
    ) {
        loop {
            // Get the action for this entry using FFI
            unsafe {
                let iter_ptr: *mut ffi::PopplerIndexIter = iter.to_glib_none().0;
                let action_ptr = ffi::poppler_index_iter_get_action(iter_ptr);

                if !action_ptr.is_null() {
                    // Cast to PopplerActionAny to get type and title
                    let action_any = &*(action_ptr as *const ffi::PopplerActionAny);

                    // Get title (replace tabs with spaces for clean display)
                    let title = if !action_any.title.is_null() {
                        CStr::from_ptr(action_any.title)
                            .to_string_lossy()
                            .replace('\t', " ")
                    } else {
                        String::new()
                    };

                    // Get page number based on action type
                    let page = if action_any.type_ == ffi::POPPLER_ACTION_GOTO_DEST {
                        let goto_dest = &*(action_ptr as *const ffi::PopplerActionGotoDest);
                        if !goto_dest.dest.is_null() {
                            let dest = &*goto_dest.dest;
                            // Check if this is a named destination (type 9 = POPPLER_DEST_NAMED)
                            if dest.type_ == 9 && !dest.named_dest.is_null() {
                                // Resolve the named destination to get the actual page
                                let doc_ptr: *mut ffi::PopplerDocument = doc.to_glib_none().0;
                                let resolved = ffi::poppler_document_find_dest(doc_ptr, dest.named_dest);
                                if !resolved.is_null() {
                                    let resolved_dest = &*resolved;
                                    let page_num = (resolved_dest.page_num.max(1) - 1) as usize;
                                    ffi::poppler_dest_free(resolved);
                                    page_num
                                } else {
                                    0
                                }
                            } else if dest.page_num > 0 {
                                // Direct page number destination
                                (dest.page_num - 1) as usize
                            } else {
                                0
                            }
                        } else {
                            0
                        }
                    } else {
                        0
                    };

                    if !title.is_empty() {
                        entries.push((title, page, level));
                    }

                    // Free the action
                    ffi::poppler_action_free(action_ptr);
                }
            }

            // Process children
            if let Some(mut child) = iter.child() {
                Self::extract_toc_entries(doc, &mut child, level + 1, entries);
            }

            // Move to next sibling
            if !iter.next() {
                break;
            }
        }
    }
}

impl Backend for PdfBackend {
    fn format_name(&self) -> &'static str {
        "PDF"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn open(&mut self, path: &Path) -> Result<()> {
        self.close();

        // poppler requires a file:// URI
        let abs_path = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf());
        let uri = format!("file://{}", abs_path.display());

        let doc = Document::from_file(&uri, None)
            .map_err(|e| anyhow!("Failed to open PDF: {}", e))?;

        self.document = Some(doc);
        self.path = Some(path.to_path_buf());
        Ok(())
    }

    fn close(&mut self) {
        self.document = None;
        self.path = None;
    }

    fn is_open(&self) -> bool {
        self.document.is_some()
    }

    fn page_count(&self) -> usize {
        self.document
            .as_ref()
            .map(|d| d.n_pages() as usize)
            .unwrap_or(0)
    }

    fn page_size(&self, page: usize) -> Result<PageSize> {
        let p = self.get_page(page)?;
        let (width, height) = p.size();
        Ok(PageSize { width, height })
    }

    fn render_page(&mut self, page: usize, scale: f64) -> Result<RenderedPage> {
        let p = self.get_page(page)?;
        let (width, height) = p.size();

        let width_px = (width * scale).ceil() as i32;
        let height_px = (height * scale).ceil() as i32;

        // Create Cairo surface for rendering
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width_px, height_px)
            .map_err(|e| anyhow!("Failed to create surface: {}", e))?;

        {
            let ctx = cairo::Context::new(&surface)
                .map_err(|e| anyhow!("Failed to create context: {}", e))?;

            // White background
            ctx.set_source_rgb(1.0, 1.0, 1.0);
            ctx.paint().map_err(|e| anyhow!("Paint failed: {}", e))?;

            // Scale and render
            ctx.scale(scale, scale);
            p.render(&ctx);
        }

        // Flush and get data
        surface
            .flush();

        let stride = surface.stride() as usize;
        let data = surface.data().map_err(|e| anyhow!("Failed to get surface data: {}", e))?;

        // Convert ARGB (Cairo) to RGBA
        let mut rgba = Vec::with_capacity((width_px * height_px * 4) as usize);
        for y in 0..height_px as usize {
            for x in 0..width_px as usize {
                let offset = y * stride + x * 4;
                // Cairo uses BGRA on little-endian systems (stored as ARGB32)
                let b = data[offset];
                let g = data[offset + 1];
                let r = data[offset + 2];
                let a = data[offset + 3];
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                rgba.push(a);
            }
        }

        Ok(RenderedPage {
            data: rgba,
            width: width_px as u32,
            height: height_px as u32,
            index: page,
        })
    }

    fn supports_search(&self) -> bool {
        true
    }

    fn search_page(&self, page: usize, query: &str) -> Vec<(f64, f64, f64, f64)> {
        let p = match self.get_page(page) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };

        p.find_text(query)
            .into_iter()
            .map(|r| (r.x1(), r.y1(), r.x2(), r.y2()))
            .collect()
    }

    fn supports_text_selection(&self) -> bool {
        true
    }

    fn get_text_for_area(&self, page: usize, area: (f64, f64, f64, f64)) -> Option<String> {
        let p = self.get_page(page).ok()?;
        let (x1, y1, x2, y2) = area;

        // Create a Rectangle for the selection area
        let mut rect = Rectangle::default();
        rect.set_x1(x1);
        rect.set_y1(y1);
        rect.set_x2(x2);
        rect.set_y2(y2);

        // Use selected_text with glyph selection style for accurate word selection
        let result = p.selected_text(SelectionStyle::Glyph, &mut rect)
            .map(|s| s.to_string());

        tracing::debug!(
            "PDF get_text_for_area: page={}, rect=({:.1},{:.1})-({:.1},{:.1}), result={:?}",
            page, x1, y1, x2, y2,
            result.as_ref().map(|s| s.chars().take(50).collect::<String>())
        );

        result
    }

    fn get_selection_region(
        &self,
        page: usize,
        area: (f64, f64, f64, f64),
        scale: f64,
    ) -> Vec<(i32, i32, i32, i32)> {
        let p = match self.get_page(page) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };

        let (x1, y1, x2, y2) = area;
        let mut rect = Rectangle::default();
        rect.set_x1(x1);
        rect.set_y1(y1);
        rect.set_x2(x2);
        rect.set_y2(y2);

        // Get the selection region from poppler - this returns the actual text bounds
        if let Some(region) = p.selected_region(scale, SelectionStyle::Glyph, &mut rect) {
            let num_rects = region.num_rectangles();
            let mut result = Vec::with_capacity(num_rects as usize);
            for i in 0..num_rects {
                let r = region.rectangle(i);
                result.push((r.x(), r.y(), r.width(), r.height()));
            }
            result
        } else {
            Vec::new()
        }
    }

    fn has_toc(&self) -> bool {
        if let Some(ref doc) = self.document {
            // Check if document has an index by creating an IndexIter
            let iter = IndexIter::new(doc);
            // IndexIter::new returns an empty iter if no TOC, but we can check
            // by trying to get an action from the first entry
            unsafe {
                let iter_ptr: *mut ffi::PopplerIndexIter = iter.to_glib_none().0;
                let action_ptr = ffi::poppler_index_iter_get_action(iter_ptr);
                let has_entries = !action_ptr.is_null();
                if !action_ptr.is_null() {
                    ffi::poppler_action_free(action_ptr);
                }
                has_entries
            }
        } else {
            false
        }
    }

    fn get_toc(&self) -> Vec<(String, usize, usize)> {
        if let Some(ref doc) = self.document {
            let mut iter = IndexIter::new(doc);
            let mut entries = Vec::new();
            Self::extract_toc_entries(doc, &mut iter, 0, &mut entries);
            entries
        } else {
            Vec::new()
        }
    }

    fn supports_links(&self) -> bool {
        self.document.is_some()
    }

    fn get_links(&self, page: usize) -> Vec<((f64, f64, f64, f64), LinkDestination)> {
        let p = match self.get_page(page) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };

        let link_mappings = p.link_mapping();
        let mut links = Vec::new();

        for mapping in link_mappings {
            unsafe {
                let mapping_ptr: *mut ffi::PopplerLinkMapping = mapping.to_glib_none().0;
                if mapping_ptr.is_null() {
                    continue;
                }

                let mapping_ffi = &*mapping_ptr;

                // Get the area rectangle
                let area = (
                    mapping_ffi.area.x1,
                    mapping_ffi.area.y1,
                    mapping_ffi.area.x2,
                    mapping_ffi.area.y2,
                );

                // Get the action
                let action_ptr = mapping_ffi.action;
                if action_ptr.is_null() {
                    continue;
                }

                let action_any = &*(action_ptr as *const ffi::PopplerActionAny);

                let dest = if action_any.type_ == ffi::POPPLER_ACTION_GOTO_DEST {
                    // Internal link to a page
                    let goto_dest = &*(action_ptr as *const ffi::PopplerActionGotoDest);
                    if !goto_dest.dest.is_null() {
                        let dest = &*goto_dest.dest;
                        // poppler uses 1-based page numbers, we use 0-based
                        Some(LinkDestination::Page((dest.page_num.max(1) - 1) as usize))
                    } else {
                        None
                    }
                } else if action_any.type_ == ffi::POPPLER_ACTION_URI {
                    // External URI link
                    let uri_action = &*(action_ptr as *const ffi::PopplerActionUri);
                    if !uri_action.uri.is_null() {
                        let uri = CStr::from_ptr(uri_action.uri).to_string_lossy().into_owned();
                        Some(LinkDestination::Uri(uri))
                    } else {
                        None
                    }
                } else if action_any.type_ == ffi::POPPLER_ACTION_NAMED {
                    // Named destination
                    let named_action = &*(action_ptr as *const ffi::PopplerActionNamed);
                    if !named_action.named_dest.is_null() {
                        let name = CStr::from_ptr(named_action.named_dest)
                            .to_string_lossy()
                            .into_owned();
                        Some(LinkDestination::Named(name))
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(dest) = dest {
                    links.push((area, dest));
                }
            }
        }

        links
    }

    fn supports_forms(&self) -> bool {
        self.document.is_some()
    }

    fn get_form_fields(&self, page: usize) -> Vec<FormFieldInfo> {
        let p = match self.get_page(page) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };

        let mappings = p.form_field_mapping();
        let mut fields = Vec::new();

        for mapping in mappings {
            unsafe {
                let mapping_ptr: *mut ffi::PopplerFormFieldMapping = mapping.to_glib_none().0;
                if mapping_ptr.is_null() {
                    continue;
                }

                let mapping_ffi = &*mapping_ptr;
                let field_ptr = mapping_ffi.field;
                if field_ptr.is_null() {
                    continue;
                }

                // Get the area rectangle
                let area = (
                    mapping_ffi.area.x1,
                    mapping_ffi.area.y1,
                    mapping_ffi.area.x2,
                    mapping_ffi.area.y2,
                );

                // Get field ID
                let field_id = ffi::poppler_form_field_get_id(field_ptr);

                // Get field name
                let name_ptr = ffi::poppler_form_field_get_name(field_ptr);
                let name = if !name_ptr.is_null() {
                    Some(CStr::from_ptr(name_ptr).to_string_lossy().into_owned())
                } else {
                    None
                };

                // Check if read-only
                let read_only = ffi::poppler_form_field_is_read_only(field_ptr) != 0;

                // Get font size
                let font_size = ffi::poppler_form_field_get_font_size(field_ptr);

                // Get field type
                let field_type_raw = ffi::poppler_form_field_get_field_type(field_ptr);

                let field_type = match field_type_raw {
                    ffi::POPPLER_FORM_FIELD_TEXT => {
                        // Get text field properties
                        let text_ptr = ffi::poppler_form_field_text_get_text(field_ptr);
                        let value = if !text_ptr.is_null() {
                            let s = CStr::from_ptr(text_ptr).to_string_lossy().into_owned();
                            cairo::glib::ffi::g_free(text_ptr as *mut _);
                            s
                        } else {
                            String::new()
                        };

                        let max_len_raw = ffi::poppler_form_field_text_get_max_len(field_ptr);
                        let max_len = if max_len_raw > 0 {
                            Some(max_len_raw)
                        } else {
                            None
                        };

                        let text_type = ffi::poppler_form_field_text_get_text_type(field_ptr);
                        let multiline = text_type == ffi::POPPLER_FORM_TEXT_MULTILINE;
                        let password = ffi::poppler_form_field_text_is_password(field_ptr) != 0;

                        FormFieldType::Text {
                            value,
                            max_len,
                            multiline,
                            password,
                        }
                    }
                    ffi::POPPLER_FORM_FIELD_BUTTON => {
                        let button_type = ffi::poppler_form_field_button_get_button_type(field_ptr);
                        let state = ffi::poppler_form_field_button_get_state(field_ptr) != 0;

                        match button_type {
                            ffi::POPPLER_FORM_BUTTON_CHECK => FormFieldType::Checkbox { checked: state },
                            ffi::POPPLER_FORM_BUTTON_RADIO => {
                                // Get radio group name (use field name or ID as group)
                                let group = name.clone().unwrap_or_else(|| field_id.to_string());
                                FormFieldType::RadioButton {
                                    group,
                                    selected: state,
                                }
                            }
                            _ => FormFieldType::Unknown,
                        }
                    }
                    ffi::POPPLER_FORM_FIELD_CHOICE => {
                        let choice_type = ffi::poppler_form_field_choice_get_choice_type(field_ptr);
                        let n_items = ffi::poppler_form_field_choice_get_n_items(field_ptr);
                        let editable = ffi::poppler_form_field_choice_is_editable(field_ptr) != 0;

                        let mut items = Vec::with_capacity(n_items as usize);
                        let mut selected = None;

                        for i in 0..n_items {
                            let item_ptr = ffi::poppler_form_field_choice_get_item(field_ptr, i);
                            if !item_ptr.is_null() {
                                let item = CStr::from_ptr(item_ptr).to_string_lossy().into_owned();
                                cairo::glib::ffi::g_free(item_ptr as *mut _);
                                items.push(item);

                                if ffi::poppler_form_field_choice_is_item_selected(field_ptr, i) != 0 {
                                    selected = Some(i);
                                }
                            }
                        }

                        FormFieldType::Dropdown {
                            items,
                            selected,
                            editable,
                        }
                    }
                    ffi::POPPLER_FORM_FIELD_SIGNATURE => FormFieldType::Signature,
                    _ => FormFieldType::Unknown,
                };

                fields.push(FormFieldInfo {
                    id: field_id,
                    page,
                    rect: area,
                    field_type,
                    name,
                    read_only,
                    font_size,
                });
            }
        }

        fields
    }

    fn set_form_field_value(&mut self, field_id: i32, value: FormFieldValue) -> Result<()> {
        let doc = self
            .document
            .as_ref()
            .ok_or_else(|| anyhow!("No document loaded"))?;

        // Get the form field by ID
        let field = doc
            .form_field(field_id)
            .ok_or_else(|| anyhow!("Form field not found: {}", field_id))?;

        match value {
            FormFieldValue::Text(text) => {
                field.text_set_text(&text);
            }
            FormFieldValue::Boolean(state) => {
                field.button_set_state(state);
            }
            FormFieldValue::ChoiceIndex(index) => {
                // Unselect all first, then select the chosen item
                field.choice_unselect_all();
                field.choice_select_item(index);
            }
        }

        Ok(())
    }

    fn save_document(&self, path: &Path) -> Result<()> {
        let doc = self
            .document
            .as_ref()
            .ok_or_else(|| anyhow!("No document loaded"))?;

        let abs_path = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf());

        // Poppler can't save over the file it has open, so save to a temp file first
        let parent = abs_path.parent().unwrap_or(Path::new("."));
        let file_name = abs_path.file_name().unwrap_or_default().to_string_lossy();
        let temp_path = parent.join(format!(".{}.tmp", file_name));
        let temp_uri = format!("file://{}", temp_path.display());

        unsafe {
            let doc_ptr: *mut ffi::PopplerDocument = doc.to_glib_none().0;
            let uri_cstring = std::ffi::CString::new(temp_uri.as_bytes())
                .map_err(|_| anyhow!("Invalid URI"))?;
            let mut error: *mut cairo::glib::ffi::GError = std::ptr::null_mut();

            let result = ffi::poppler_document_save(
                doc_ptr,
                uri_cstring.as_ptr() as *const u8,
                &mut error,
            );

            if result == 0 {
                if !error.is_null() {
                    let msg = CStr::from_ptr((*error).message).to_string_lossy().into_owned();
                    cairo::glib::ffi::g_error_free(error);
                    return Err(anyhow!("Failed to save PDF: {}", msg));
                }
                return Err(anyhow!("Failed to save PDF (unknown error)"));
            }
        }

        // Rename temp file to target path
        std::fs::rename(&temp_path, &abs_path)
            .map_err(|e| anyhow!("Failed to rename temp file: {}", e))?;

        Ok(())
    }
}
