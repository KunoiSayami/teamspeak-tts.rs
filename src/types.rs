#[derive(Clone, PartialEq)]
pub(crate) enum MainEvent {
    Exit,
}

impl MainEvent {
    pub fn is_not_exit(&self) -> bool {
        self != &Self::Exit
    }
}

#[derive(Clone)]
pub(crate) struct AdditionalArguments {
    pub(crate) server: Option<String>,
    pub(crate) web: Option<String>,
    pub(crate) leveldb: Option<String>,
}

impl AdditionalArguments {
    pub(crate) fn new(matches: &clap::ArgMatches) -> Self {
        Self {
            server: matches.get_one("server").cloned(),
            web: matches.get_one("web").cloned(),
            leveldb: matches.get_one("leveldb").cloned(),
        }
    }
}
