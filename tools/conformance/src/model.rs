//! Private policy and fixture model for `conformancecheck`.
//!
//! These types deliberately live in the tool: shipping modules expose only
//! factual probes and never depend on conformance policy types.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Convention {
    EnvValidation,
    InputByteCaps,
    InfraOutage503,
    ArgonParity,
}

impl Convention {
    pub const ALL: [Convention; 4] = [
        Convention::EnvValidation,
        Convention::InputByteCaps,
        Convention::InfraOutage503,
        Convention::ArgonParity,
    ];
}

#[derive(Clone)]
pub struct Entry {
    pub module: &'static str,
    pub stances: Vec<(Convention, Stance)>,
}

impl Entry {
    pub fn stance(&self, convention: Convention) -> Option<&Stance> {
        self.stances
            .iter()
            .find(|(candidate, _)| *candidate == convention)
            .map(|(_, stance)| stance)
    }
}

#[derive(Clone)]
pub enum Stance {
    Applies(Fixture),
    NotApplicable {
        why: &'static str,
    },
    #[allow(dead_code)]
    KnownGap {
        why: &'static str,
        remediation: &'static str,
    },
}

#[derive(Clone)]
pub enum Fixture {
    EnvValidation(Vec<EnvCase>),
    InputByteCaps(Vec<CapCase>),
    InfraOutage503(Vec<OutageCase>),
    ArgonParity(ArgonParams),
}

#[derive(Clone, Copy, Debug)]
pub struct EnvCase {
    pub var: &'static str,
    pub bad_value: &'static str,
}

#[derive(Clone)]
pub struct CapCase {
    pub name: &'static str,
    pub cap: usize,
    pub probe: Arc<dyn Fn(usize) -> bool + Send + Sync>,
}

#[derive(Clone)]
pub struct OutageCase {
    pub name: &'static str,
    pub probe: Arc<dyn Fn() -> BoxFuture<OutageClass> + Send + Sync>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OutageClass {
    Unavailable,
    Rejected,
    Other(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ArgonParams {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub output_len: usize,
}
