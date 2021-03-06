// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Def-use analysis.

use rustc::mir::repr::{Local, Location, Lvalue, Mir};
use rustc::mir::visit::{LvalueContext, MutVisitor, Visitor};
use rustc_data_structures::indexed_vec::{Idx, IndexVec};
use std::marker::PhantomData;
use std::mem;

pub struct DefUseAnalysis<'tcx> {
    info: IndexVec<Local, Info<'tcx>>,
    mir_summary: MirSummary,
}

#[derive(Clone)]
pub struct Info<'tcx> {
    pub defs_and_uses: Vec<Use<'tcx>>,
}

#[derive(Clone)]
pub struct Use<'tcx> {
    pub context: LvalueContext<'tcx>,
    pub location: Location,
}

impl<'tcx> DefUseAnalysis<'tcx> {
    pub fn new(mir: &Mir<'tcx>) -> DefUseAnalysis<'tcx> {
        DefUseAnalysis {
            info: IndexVec::from_elem_n(Info::new(), mir.count_locals()),
            mir_summary: MirSummary::new(mir),
        }
    }

    pub fn analyze(&mut self, mir: &Mir<'tcx>) {
        let mut finder = DefUseFinder {
            info: mem::replace(&mut self.info, IndexVec::new()),
            mir_summary: self.mir_summary,
        };
        finder.visit_mir(mir);
        self.info = finder.info
    }

    pub fn local_info(&self, local: Local) -> &Info<'tcx> {
        &self.info[local]
    }

    pub fn local_info_mut(&mut self, local: Local) -> &mut Info<'tcx> {
        &mut self.info[local]
    }

    fn mutate_defs_and_uses<F>(&self, local: Local, mir: &mut Mir<'tcx>, mut callback: F)
                               where F: for<'a> FnMut(&'a mut Lvalue<'tcx>,
                                                      LvalueContext<'tcx>,
                                                      Location) {
        for lvalue_use in &self.info[local].defs_and_uses {
            MutateUseVisitor::new(local,
                                  &mut callback,
                                  self.mir_summary,
                                  mir).visit_location(mir, lvalue_use.location)
        }
    }

    /// FIXME(pcwalton): This should update the def-use chains.
    pub fn replace_all_defs_and_uses_with(&self,
                                          local: Local,
                                          mir: &mut Mir<'tcx>,
                                          new_lvalue: Lvalue<'tcx>) {
        self.mutate_defs_and_uses(local, mir, |lvalue, _, _| *lvalue = new_lvalue.clone())
    }
}

struct DefUseFinder<'tcx> {
    info: IndexVec<Local, Info<'tcx>>,
    mir_summary: MirSummary,
}

impl<'tcx> DefUseFinder<'tcx> {
    fn lvalue_mut_info(&mut self, lvalue: &Lvalue<'tcx>) -> Option<&mut Info<'tcx>> {
        let info = &mut self.info;
        self.mir_summary.local_index(lvalue).map(move |local| &mut info[local])
    }
}

impl<'tcx> Visitor<'tcx> for DefUseFinder<'tcx> {
    fn visit_lvalue(&mut self,
                    lvalue: &Lvalue<'tcx>,
                    context: LvalueContext<'tcx>,
                    location: Location) {
        if let Some(ref mut info) = self.lvalue_mut_info(lvalue) {
            info.defs_and_uses.push(Use {
                context: context,
                location: location,
            })
        }
        self.super_lvalue(lvalue, context, location)
    }
}

impl<'tcx> Info<'tcx> {
    fn new() -> Info<'tcx> {
        Info {
            defs_and_uses: vec![],
        }
    }

    pub fn def_count(&self) -> usize {
        self.defs_and_uses.iter().filter(|lvalue_use| lvalue_use.context.is_mutating_use()).count()
    }

    pub fn def_count_not_including_drop(&self) -> usize {
        self.defs_and_uses.iter().filter(|lvalue_use| {
            lvalue_use.context.is_mutating_use() && !lvalue_use.context.is_drop()
        }).count()
    }

    pub fn use_count(&self) -> usize {
        self.defs_and_uses.iter().filter(|lvalue_use| {
            lvalue_use.context.is_nonmutating_use()
        }).count()
    }
}

struct MutateUseVisitor<'tcx, F> {
    query: Local,
    callback: F,
    mir_summary: MirSummary,
    phantom: PhantomData<&'tcx ()>,
}

impl<'tcx, F> MutateUseVisitor<'tcx, F> {
    fn new(query: Local, callback: F, mir_summary: MirSummary, _: &Mir<'tcx>)
           -> MutateUseVisitor<'tcx, F>
           where F: for<'a> FnMut(&'a mut Lvalue<'tcx>, LvalueContext<'tcx>, Location) {
        MutateUseVisitor {
            query: query,
            callback: callback,
            mir_summary: mir_summary,
            phantom: PhantomData,
        }
    }
}

impl<'tcx, F> MutVisitor<'tcx> for MutateUseVisitor<'tcx, F>
              where F: for<'a> FnMut(&'a mut Lvalue<'tcx>, LvalueContext<'tcx>, Location) {
    fn visit_lvalue(&mut self,
                    lvalue: &mut Lvalue<'tcx>,
                    context: LvalueContext<'tcx>,
                    location: Location) {
        if self.mir_summary.local_index(lvalue) == Some(self.query) {
            (self.callback)(lvalue, context, location)
        }
        self.super_lvalue(lvalue, context, location)
    }
}

/// A small structure that enables various metadata of the MIR to be queried
/// without a reference to the MIR itself.
#[derive(Clone, Copy)]
struct MirSummary {
    arg_count: usize,
    var_count: usize,
    temp_count: usize,
}

impl MirSummary {
    fn new(mir: &Mir) -> MirSummary {
        MirSummary {
            arg_count: mir.arg_decls.len(),
            var_count: mir.var_decls.len(),
            temp_count: mir.temp_decls.len(),
        }
    }

    fn local_index<'tcx>(&self, lvalue: &Lvalue<'tcx>) -> Option<Local> {
        match *lvalue {
            Lvalue::Arg(arg) => Some(Local::new(arg.index())),
            Lvalue::Var(var) => Some(Local::new(var.index() + self.arg_count)),
            Lvalue::Temp(temp) => {
                Some(Local::new(temp.index() + self.arg_count + self.var_count))
            }
            Lvalue::ReturnPointer => {
                Some(Local::new(self.arg_count + self.var_count + self.temp_count))
            }
            _ => None,
        }
    }
}

