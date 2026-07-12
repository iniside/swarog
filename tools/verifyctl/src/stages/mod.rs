pub mod audit;
pub mod command;
pub mod fortress;
pub mod splitproof;

use crate::model::{StageClass, StageId};

pub type StageFn = fn(&mut crate::runner::Context<'_>) -> anyhow::Result<crate::model::Outcome>;

#[derive(Clone, Copy)]
pub struct Stage {
    pub id: StageId,
    pub class: StageClass,
    pub run: StageFn,
}

pub const INITIAL: &[Stage] = &[
    Stage {
        id: StageId::Build,
        class: StageClass::Blocking,
        run: command::build,
    },
    Stage {
        id: StageId::Clippy,
        class: StageClass::Blocking,
        run: command::clippy,
    },
    Stage {
        id: StageId::Test,
        class: StageClass::Blocking,
        run: command::test,
    },
    Stage {
        id: StageId::Audit,
        class: StageClass::Blocking,
        run: audit::run,
    },
    Stage {
        id: StageId::Fortress,
        class: StageClass::Blocking,
        run: fortress::run,
    },
    Stage {
        id: StageId::Routecheck,
        class: StageClass::Blocking,
        run: command::routecheck,
    },
    Stage {
        id: StageId::SplitProof,
        class: StageClass::Blocking,
        run: splitproof::run,
    },
];
