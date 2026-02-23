use crate::timeline::Timeline;

const MAX_HISTORY: usize = 100;

#[derive(Debug, Clone)]
pub struct UndoManager {
    undo_stack: Vec<Timeline>,
    redo_stack: Vec<Timeline>,
}

impl UndoManager {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn save(&mut self, snapshot: Timeline) {
        self.undo_stack.push(snapshot);
        self.redo_stack.clear();
        if self.undo_stack.len() > MAX_HISTORY {
            self.undo_stack.remove(0);
        }
    }

    pub fn undo(&mut self, current: Timeline) -> Option<Timeline> {
        let previous = self.undo_stack.pop()?;
        self.redo_stack.push(current);
        Some(previous)
    }

    pub fn redo(&mut self, current: Timeline) -> Option<Timeline> {
        let next = self.redo_stack.pop()?;
        self.undo_stack.push(current);
        Some(next)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }
}

impl Default for UndoManager {
    fn default() -> Self {
        Self::new()
    }
}
