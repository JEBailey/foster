//! Choose failure-state caching from the completed machine control-flow graph.
use super::{ClifBlock, HashMap, HashSet, InstBuilder};
use cranelift_codegen::{
    dominator_tree::DominatorTree,
    flowgraph::ControlFlowGraph,
    ir::{FuncRef, Function, Inst},
};

struct Paths {
    blocks: Vec<ClifBlock>,
    indices: HashMap<ClifBlock, usize>,
    successors: Vec<Vec<usize>>,
    checks: Vec<usize>,
    incoming: Vec<Option<u8>>,
}

impl Paths {
    fn new(function: &Function, reads: &[Inst]) -> Self {
        let blocks = function.layout.blocks().collect::<Vec<_>>();
        let indices: HashMap<_, _> = blocks.iter().enumerate().map(|(i, b)| (*b, i)).collect();
        let successors = blocks
            .iter()
            .map(|block| {
                function
                    .layout
                    .block_insts(*block)
                    .flat_map(|inst| {
                        function.dfg.insts[inst].branch_destination(
                            &function.dfg.jump_tables,
                            &function.dfg.exception_tables,
                        )
                    })
                    .map(|edge| indices[&edge.block(&function.dfg.value_lists)])
                    .collect()
            })
            .collect();
        let mut checks = vec![0; blocks.len()];
        for read in reads {
            checks[indices[&function
                .layout
                .inst_block(*read)
                .expect("failure read block")]] += 1;
        }
        let incoming = vec![None; blocks.len()];
        Self {
            blocks,
            indices,
            successors,
            checks,
            incoming,
        }
    }

    fn can_repeat(&mut self) -> bool {
        // Maximum checks along a reachable path, saturated at two. Merges take
        // max rather than sum, so exclusive branches each containing one check
        // do not justify caching. A loop revisiting one check reaches two.
        let mut work = vec![0];
        self.incoming[0] = Some(0);
        while let Some(block) = work.pop() {
            let count =
                (usize::from(self.incoming[block].unwrap()) + self.checks[block]).min(2) as u8;
            for next in &self.successors[block] {
                if self.incoming[*next].is_none_or(|old| old < count) {
                    self.incoming[*next] = Some(count);
                    work.push(*next);
                }
            }
        }
        self.incoming
            .iter()
            .zip(&self.checks)
            .any(|(before, count)| before.is_some_and(|before| usize::from(before) + count >= 2))
    }

    fn cyclic(&self, block: ClifBlock) -> bool {
        let start = self.indices[&block];
        let mut visited = HashSet::new();
        let mut work = self.successors[start].clone();
        while let Some(next) = work.pop() {
            if next == start {
                return true;
            }
            if visited.insert(next) {
                work.extend_from_slice(&self.successors[next]);
            }
        }
        false
    }
}

pub(super) fn select(function: &mut Function, acquire: Inst, reads: &[Inst], direct: FuncRef) {
    let mut paths = Paths::new(function, reads);
    if !paths.can_repeat() {
        for read in reads {
            function.replace(*read).call(direct, &[]);
        }
        function.layout.remove_inst(acquire);
        crate::compiler::profile::count("native.failure_cache.direct");
        return;
    }
    // Dead blocks must not retain uses of a handle moved under a live guard.
    for read in reads {
        let block = function
            .layout
            .inst_block(*read)
            .expect("failure read block");
        if paths.incoming[paths.indices[&block]].is_none() {
            function.replace(*read).call(direct, &[]);
        }
    }

    let cfg = ControlFlowGraph::with_function(function);
    let dominators = DominatorTree::with_function(function, &cfg);
    let mut checked_blocks = paths
        .blocks
        .iter()
        .enumerate()
        .filter(|(index, _)| paths.incoming[*index].is_some() && paths.checks[*index] != 0)
        .map(|(_, block)| *block);
    let mut placement = checked_blocks.next().expect("reachable repeated checks");
    for block in checked_blocks {
        while !dominators.dominates(placement, block, &function.layout) {
            placement = dominators.idom(placement).expect("common entry dominator");
        }
    }
    // A dominating block inside a cycle could execute repeatedly. Walk outward
    // until acquisition executes at most once, retaining coroutine-frame scope.
    while paths.cyclic(placement) {
        placement = dominators
            .idom(placement)
            .expect("loop has an entry dominator");
    }
    function.layout.remove_inst(acquire);
    if let Some(first) = function.layout.first_inst(placement) {
        function.layout.insert_inst(acquire, first);
    } else {
        function.layout.append_inst(acquire, placement);
    }
    crate::compiler::profile::count("native.failure_cache.cached");
    if Some(placement) != function.layout.entry_block() {
        // This counts placement below entry; unconditional entry jumps can
        // also qualify, so it does not imply setup is skipped on some path.
        crate::compiler::profile::count("native.failure_cache.guarded");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{
        FunctionBuilder, FunctionBuilderContext, Module, NativeType, ObjectModule, ir,
        propagate_native_failure,
    };

    #[derive(Clone, Copy, Debug)]
    enum Shape {
        Single,
        Exclusive,
        Sequential,
        Loop,
        Guarded,
        GuardedLoop,
        EmptyLoop,
        DeadChecks,
        GuardedWithDeadCheck,
        Irreducible,
    }

    fn check(shape: Shape, cached: bool, guarded: bool) {
        let isa = cranelift_native::builder()
            .unwrap()
            .finish(cranelift_codegen::settings::Flags::new(
                cranelift_codegen::settings::builder(),
            ))
            .unwrap();
        let mut module = ObjectModule::new(
            cranelift_object::ObjectBuilder::new(
                isa,
                "cache-analysis-test",
                cranelift_module::default_libcall_names(),
            )
            .unwrap(),
        );
        let mut context = module.make_context();
        context.func.signature = crate::native::signature(
            &mut module,
            &ir::Signature {
                parameters: vec![NativeType::Bool],
                result: NativeType::Int,
            },
        );
        let id = module
            .declare_function(
                "case",
                cranelift_module::Linkage::Local,
                &context.func.signature,
            )
            .unwrap();
        let mut builder_context = FunctionBuilderContext::new();
        let (acquire, direct, reads, expected_block);
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let entry = builder.create_block();
            let left = builder.create_block();
            let right = builder.create_block();
            let header = builder.create_block();
            let body = builder.create_block();
            let done = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let flag = builder.block_params(entry)[0];
            let answer = builder.ins().iconst(cranelift_codegen::ir::types::I64, 42);
            let query = |builder: &mut FunctionBuilder<'_>, module: &mut ObjectModule| {
                propagate_native_failure(builder, module).unwrap()
            };
            match shape {
                Shape::Single | Shape::Sequential => {
                    query(&mut builder, &mut module);
                    if matches!(shape, Shape::Sequential) {
                        query(&mut builder, &mut module);
                    }
                    builder.ins().return_(&[answer]);
                }
                Shape::Exclusive => {
                    builder.ins().brif(flag, left, &[], right, &[]);
                    for arm in [left, right] {
                        builder.switch_to_block(arm);
                        query(&mut builder, &mut module);
                        builder.ins().return_(&[answer]);
                    }
                }
                Shape::Guarded | Shape::GuardedWithDeadCheck => {
                    builder.ins().brif(flag, left, &[], done, &[]);
                    builder.switch_to_block(left);
                    query(&mut builder, &mut module);
                    query(&mut builder, &mut module);
                    builder.ins().return_(&[answer]);
                    builder.switch_to_block(done);
                    builder.ins().return_(&[answer]);
                    if matches!(shape, Shape::GuardedWithDeadCheck) {
                        builder.switch_to_block(right);
                        query(&mut builder, &mut module);
                        builder.ins().return_(&[answer]);
                    }
                }
                Shape::Loop | Shape::GuardedLoop | Shape::EmptyLoop => {
                    if matches!(shape, Shape::EmptyLoop) {
                        query(&mut builder, &mut module);
                    }
                    if matches!(shape, Shape::GuardedLoop) {
                        builder.ins().brif(flag, left, &[], done, &[]);
                        builder.switch_to_block(left);
                    }
                    builder.ins().jump(header, &[]);
                    builder.switch_to_block(header);
                    builder.ins().brif(flag, body, &[], done, &[]);
                    builder.switch_to_block(body);
                    if !matches!(shape, Shape::EmptyLoop) {
                        query(&mut builder, &mut module);
                    }
                    builder.ins().jump(header, &[]);
                    builder.switch_to_block(done);
                    builder.ins().return_(&[answer]);
                }
                Shape::DeadChecks => {
                    builder.ins().return_(&[answer]);
                    builder.switch_to_block(left);
                    query(&mut builder, &mut module);
                    query(&mut builder, &mut module);
                    builder.ins().return_(&[answer]);
                }
                Shape::Irreducible => {
                    builder.ins().brif(flag, left, &[], right, &[]);
                    builder.switch_to_block(left);
                    query(&mut builder, &mut module);
                    builder.ins().brif(flag, right, &[], done, &[]);
                    builder.switch_to_block(right);
                    builder.ins().brif(flag, left, &[], done, &[]);
                    builder.switch_to_block(done);
                    builder.ins().return_(&[answer]);
                }
            }
            let state = builder.failure_state.unwrap();
            let cranelift_codegen::ir::ValueDef::Result(inst, _) =
                builder.func.dfg.value_def(state)
            else {
                panic!("state acquisition")
            };
            acquire = inst;
            direct = builder.uncached_failure.unwrap();
            reads = builder.failure_reads.clone();
            expected_block = if guarded { left } else { entry };
            builder.seal_all_blocks();
            builder.finalize(module.target_config());
        }
        assert_eq!(
            context.func.layout.inst_block(acquire),
            cached.then_some(expected_block),
            "{shape:?}"
        );
        if !cached {
            for read in reads {
                let cranelift_codegen::ir::InstructionData::Call { func_ref, .. } =
                    context.func.dfg.insts[read]
                else {
                    panic!("status call")
                };
                assert_eq!(func_ref, direct, "{shape:?}");
                assert_eq!(context.func.dfg.inst_args(read).len(), 0);
            }
        }
        module
            .define_function(id, &mut context)
            .unwrap_or_else(|error| panic!("{shape:?}: {error}"));
    }

    #[test]
    fn single_check_uses_direct_lookup() {
        check(Shape::Single, false, false);
    }
    #[test]
    fn exclusive_checks_are_not_added_together() {
        check(Shape::Exclusive, false, false);
    }
    #[test]
    fn sequential_checks_reuse_a_handle() {
        check(Shape::Sequential, true, false);
    }
    #[test]
    fn one_check_in_a_loop_reuses_a_handle() {
        check(Shape::Loop, true, false);
    }
    #[test]
    fn repeated_checks_under_a_guard_acquire_after_the_guard() {
        check(Shape::Guarded, true, true);
    }
    #[test]
    fn guarded_loop_acquires_once_outside_the_cycle() {
        check(Shape::GuardedLoop, true, true);
    }
    #[test]
    fn loops_without_checks_do_not_justify_caching() {
        check(Shape::EmptyLoop, false, false);
    }
    #[test]
    fn unreachable_checks_do_not_justify_caching() {
        check(Shape::DeadChecks, false, false);
    }
    #[test]
    fn unreachable_uses_do_not_prevent_guarded_placement() {
        check(Shape::GuardedWithDeadCheck, true, true);
    }
    #[test]
    fn irreducible_cycles_are_detected() {
        check(Shape::Irreducible, true, false);
    }
}
