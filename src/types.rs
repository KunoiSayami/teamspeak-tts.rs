#[derive(Clone, PartialEq)]
pub(crate) enum MainEvent {
    Exit,
}

impl MainEvent {
    pub fn is_not_exit(self) -> bool {
        self != Self::Exit
    }
}
