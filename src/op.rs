//! The one cancellation primitive for long operations.
//!
//! Every [`Bot`](crate::Bot) has a root token that the bridge cancels when
//! the connection ends. An [`OpSlot`] holds the one running operation of a
//! kind (an attack, a dig, an eat): starting a new one cancels the old one,
//! and each operation's token is a child of the root, so a disconnect
//! cancels every operation too.

use tokio_util::sync::CancellationToken;

/// One started operation: its token, and which start it was.
#[derive(Clone, Debug)]
pub struct Op {
    id: u64,
    pub(crate) token: CancellationToken,
}

impl Op {
    /// A newer start, a cancel, or the end of the connection ended it.
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Resolves when [`Self::is_cancelled`] becomes true.
    pub async fn cancelled(&self) {
        self.token.cancelled().await
    }
}

/// The running operation of one kind.
#[derive(Debug, Default)]
pub struct OpSlot {
    started: u64,
    current: Option<Op>,
}

impl OpSlot {
    /// Cancel the running operation, if any, and start a new one under
    /// `root`.
    pub(crate) fn start(&mut self, root: &CancellationToken) -> Op {
        self.cancel();
        self.started += 1;
        let op = Op {
            id: self.started,
            token: root.child_token(),
        };
        self.current = Some(op.clone());
        op
    }

    /// Cancel the running operation, if any.
    pub fn cancel(&mut self) {
        if let Some(op) = self.current.take() {
            op.token.cancel();
        }
    }

    /// End `op` when it is still the running operation. `true` means the
    /// caller owns the outcome; `false` means a newer start or a
    /// [`Self::cancel`] already ended it.
    pub fn finish(&mut self, op: &Op) -> bool {
        let current = self.current.as_ref().is_some_and(|c| c.id == op.id);
        if current {
            self.current = None;
        }
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_start_cancels_the_old_operation() {
        let root = CancellationToken::new();
        let mut slot = OpSlot::default();
        let first = slot.start(&root);
        let second = slot.start(&root);
        assert!(first.token.is_cancelled());
        assert!(!second.token.is_cancelled());
        assert!(!slot.finish(&first), "the first was superseded");
        assert!(slot.finish(&second));
        assert!(!slot.finish(&second), "an operation finishes once");
    }

    #[test]
    fn cancel_ends_the_operation_without_an_owner() {
        let root = CancellationToken::new();
        let mut slot = OpSlot::default();
        let op = slot.start(&root);
        slot.cancel();
        assert!(op.token.is_cancelled());
        assert!(!slot.finish(&op));
    }

    #[test]
    fn the_root_cancels_every_operation_and_keeps_the_owner() {
        let root = CancellationToken::new();
        let (mut a, mut b) = (OpSlot::default(), OpSlot::default());
        let (op_a, op_b) = (a.start(&root), b.start(&root));
        root.cancel();
        assert!(op_a.token.is_cancelled() && op_b.token.is_cancelled());
        // a disconnect leaves the outcome to the operation itself
        assert!(a.finish(&op_a));
        assert!(b.finish(&op_b));
    }
}
