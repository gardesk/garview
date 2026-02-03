//! Undo/redo history for annotations.

/// Undo/redo history stack.
pub struct History {
    /// Past states (for undo).
    undo_stack: Vec<Vec<u8>>,
    /// Future states (for redo).
    redo_stack: Vec<Vec<u8>>,
    /// Maximum number of states to keep.
    max_entries: usize,
}

impl History {
    /// Create a new history with the given max entries.
    pub fn new(max_entries: usize) -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_entries,
        }
    }

    /// Push a new state (called before making a change).
    pub fn push(&mut self, state: Vec<u8>) {
        // Clear redo stack on new action
        self.redo_stack.clear();

        // Add to undo stack
        self.undo_stack.push(state);

        // Trim to max entries
        while self.undo_stack.len() > self.max_entries {
            self.undo_stack.remove(0);
        }
    }

    /// Pop the last state for undo.
    pub fn undo(&mut self, current_state: Vec<u8>) -> Option<Vec<u8>> {
        if let Some(state) = self.undo_stack.pop() {
            // Save current state for redo
            self.redo_stack.push(current_state);
            Some(state)
        } else {
            None
        }
    }

    /// Pop from redo stack.
    pub fn redo(&mut self, current_state: Vec<u8>) -> Option<Vec<u8>> {
        if let Some(state) = self.redo_stack.pop() {
            // Save current state for undo
            self.undo_stack.push(current_state);
            Some(state)
        } else {
            None
        }
    }

    /// Check if undo is available.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Check if redo is available.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Clear all history.
    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    /// Get number of undo states.
    pub fn undo_count(&self) -> usize {
        self.undo_stack.len()
    }

    /// Get number of redo states.
    pub fn redo_count(&self) -> usize {
        self.redo_stack.len()
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new(50)
    }
}
