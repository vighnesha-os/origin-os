//! Host tests of the policy (CLAUDE.md §72; TASKS 18b's required tests).
//!
//! `world` is a [`crate::Platform`] made of scripted processors: each has its own two
//! instructions, which give a known sequence and can be told to fail in each of the ways the
//! policy must handle. A test can move the calling thread to another processor between steps.

mod health;
mod model;
mod reference;
mod requests;
mod schedule;
mod selftest;
mod start;
pub(crate) mod world;
