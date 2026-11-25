// Copyright (c) 2025 Hakai Offensive Security.
// SPDX-License-Identifier: GPL-3.0-or-later

#include "llvm-c/Core.h"
#include "llvm/ADT/SCCIterator.h"
#include "llvm/ADT/SmallPtrSet.h"
#include "llvm/IR/CFG.h"
#include "llvm/IR/Dominators.h"
#include "llvm/IR/Function.h"
#include "llvm/IR/Instructions.h"
#include "llvm/IR/Module.h"
#include "llvm/Support/CBindingWrapping.h"
#include "llvm/Transforms/Utils/CodeExtractor.h"
#include <cstdint>

using namespace llvm;

namespace {
// Keep the order and layout in sync with block_outline.rs. Counts describe
// original blocks; CodeExtractor's split/replacement blocks are never counted.
enum Reason : unsigned {
  UnsafeFunction, Entry, Unreachable, Cyclic, Terminator, RegionInstruction,
  TooSmall, BoundaryType, BoundaryBudget, ExtractorIneligible, SelectionBudget,
  ReasonCount
};
struct OutlineStats {
  uint64_t eligible;
  uint64_t skipped[ReasonCount];
};

bool unsafeFunction(const Function &F) {
  if (F.getCallingConv() != CallingConv::C &&
      F.getCallingConv() != CallingConv::Fast &&
      F.getCallingConv() != CallingConv::Cold)
    return true;
  if (F.isVarArg() || F.hasGC() || F.hasPersonalityFn() ||
      F.getAddressSpace() != 0 || F.hasFnAttribute(Attribute::Naked) ||
      F.hasFnAttribute(Attribute::ReturnsTwice) ||
      F.hasFnAttribute(Attribute::Convergent) ||
      F.hasFnAttribute(Attribute::PresplitCoroutine) ||
      F.hasFnAttribute("thunk"))
    return true;
  // These ABI values cannot simply become ordinary scalar helper arguments.
  for (const Argument &A : F.args())
    if (A.hasAttribute(Attribute::InAlloca) ||
        A.hasAttribute(Attribute::Preallocated) ||
        A.hasAttribute(Attribute::SwiftError) ||
        A.hasAttribute(Attribute::SwiftAsync) ||
        A.hasAttribute(Attribute::Nest))
      return true;
  for (const BasicBlock &BB : F) {
    if (BB.hasAddressTaken() || BB.isEHPad())
      return true;
    for (const Instruction &I : BB) {
      if (isa<IndirectBrInst, InvokeInst, CallBrInst, ResumeInst,
              CatchReturnInst, CleanupReturnInst, CatchSwitchInst>(I))
        return true;
      if (const auto *A = dyn_cast<AllocaInst>(&I))
        if (A->isUsedWithInAlloca() || A->isSwiftError())
          return true;
      const auto *CB = dyn_cast<CallBase>(&I);
      if (!CB)
        continue;
      if (CB->isInlineAsm() || CB->hasFnAttr(Attribute::ReturnsTwice) ||
          CB->isConvergent() || CB->hasOperandBundles())
        return true;
      if (const auto *CI = dyn_cast<CallInst>(CB))
        if (CI->isMustTailCall())
          return true;
      if (const Function *Callee = CB->getCalledFunction()) {
        if (Callee->hasFnAttribute(Attribute::ReturnsTwice))
          return true;
        StringRef N = Callee->getName();
        // Frame identity, nonlocal control flow, stack lifetime and statepoint
        // conventions are function-wide constraints, not just region checks.
        if (N.starts_with("llvm.coro.") || N.starts_with("llvm.eh.") ||
            N.starts_with("llvm.gc") || N.starts_with("llvm.experimental.gc.") ||
            N.starts_with("llvm.experimental.deoptimize") ||
            N.starts_with("llvm.experimental.guard") ||
            N.starts_with("llvm.experimental.stackmap") ||
            N.starts_with("llvm.experimental.patchpoint.") ||
            N.starts_with("llvm.experimental.convergence.") ||
            N.starts_with("llvm.frameaddress") ||
            N.starts_with("llvm.returnaddress") ||
            N.starts_with("llvm.addressofreturnaddress") ||
            N.starts_with("llvm.localaddress") ||
            N.starts_with("llvm.get.dynamic.area.offset") ||
            N.starts_with("llvm.stackaddress") || N.starts_with("llvm.stackguard") ||
            N.starts_with("llvm.read_volatile_register") ||
            N.starts_with("llvm.localescape") || N.starts_with("llvm.localrecover") ||
            N.starts_with("llvm.stacksave") || N.starts_with("llvm.stackrestore") ||
            N.starts_with("llvm.stackprotector") || N.starts_with("llvm.sponentry") ||
            N.starts_with("llvm.read_register") || N.starts_with("llvm.write_register"))
          return true;
      }
    }
  }
  return false;
}

bool boundaryType(Type *T, const DataLayout &DL) {
  if (auto *IT = dyn_cast<IntegerType>(T))
    return IT->getBitWidth() <= 128;
  if (T->isFloatTy() || T->isDoubleTy())
    return true;
  if (auto *PT = dyn_cast<PointerType>(T))
    return PT->getAddressSpace() == 0 && !DL.isNonIntegralPointerType(PT);
  return false;
}

// SplitMix64 and rejection sampling select uniformly without a candidate-order
// dependency on pointer addresses or another pass's random stream.
uint64_t nextRandom(uint64_t &State) {
  uint64_t Z = (State += UINT64_C(0x9e3779b97f4a7c15));
  Z = (Z ^ (Z >> 30)) * UINT64_C(0xbf58476d1ce4e5b9);
  Z = (Z ^ (Z >> 27)) * UINT64_C(0x94d049bb133111eb);
  return Z ^ (Z >> 31);
}
} // namespace

// Returns 0 (no eligible region), 1 (one extraction), -1 (unexpected failure).
// FRef is a live defined Function. Stats is optional and zero-initialized here.
extern "C" int32_t lyra_outline_function(LLVMValueRef FRef, uint64_t Seed,
                                         OutlineStats *Stats) {
  if (Stats)
    *Stats = {};
  auto *F = dyn_cast<Function>(unwrap(FRef));
  if (!F || F->isDeclaration())
    return -1;
  const bool SingleBlock = F->getEntryBlock().getNextNode() == nullptr;
  if (SingleBlock && !Stats)
    return 0; // No non-entry candidate; skip safety/CFG analysis when unreported.
  auto Skip = [&](Reason R, uint64_t N = 1) {
    if (Stats)
      Stats->skipped[R] += N;
  };
  // Output slots must live in the ordinary stack address space. Unusual
  // targets require their own ABI evidence, not a host-derived assumption.
  if (unsafeFunction(*F) || F->getDataLayout().getAllocaAddrSpace() != 0) {
    Skip(UnsafeFunction, F->size());
    return 0;
  }
  if (SingleBlock) {
    Skip(Entry);
    return 0;
  }
  DominatorTree DT(*F);
  SmallPtrSet<BasicBlock *, 32> Cycles;
  // SCC iteration covers irreducible multi-entry cycles as well as natural
  // loops. Unreachable blocks are separately rejected using dominance.
  for (auto SI = scc_begin(F); !SI.isAtEnd(); ++SI)
    if (SI.hasCycle())
      for (BasicBlock *BB : *SI)
        Cycles.insert(BB);

  BasicBlock *Selected = nullptr;
  uint64_t Eligible = 0;
  const DataLayout &DL = F->getDataLayout();
  for (BasicBlock &BB : *F) {
    if (&BB == &F->getEntryBlock()) {
      Skip(Entry);
      continue;
    }
    if (!DT.isReachableFromEntry(&BB)) {
      Skip(Unreachable);
      continue;
    }
    if (Cycles.contains(&BB)) {
      Skip(Cyclic);
      continue;
    }
    if (!isa<BranchInst>(BB.getTerminator())) {
      Skip(Terminator);
      continue;
    }
    unsigned Instructions = 0;
    bool Unsupported = false;
    for (Instruction &I : BB) {
      if (isa<CallBase, AllocaInst, VAArgInst>(I) || I.getType()->isTokenTy() ||
          isa<ScalableVectorType>(I.getType())) {
        Unsupported = true;
        break;
      }
      if (!isa<PHINode>(I) && !I.isTerminator())
        ++Instructions;
    }
    if (Unsupported) {
      Skip(RegionInstruction);
      continue;
    }
    if (Instructions < 8) {
      Skip(TooSmall);
      continue;
    }
    CodeExtractor CE({&BB}, &DT, false, nullptr, nullptr, nullptr, false,
                     false, nullptr, "lyra_outline");
    if (!CE.isEligible()) {
      Skip(ExtractorIneligible);
      continue;
    }
    SetVector<Value *> Inputs, Outputs, Allocas;
    CE.findInputsOutputs(Inputs, Outputs, Allocas);
    // With multiple predecessors CodeExtractor leaves entry PHIs in the
    // caller. Include those prospective inputs as well. Counting their old
    // operands too is conservative, never understates the six-input cap.
    for (PHINode &PN : BB.phis())
      if (PN.getNumIncomingValues() > 1)
        Inputs.insert(&PN);
    bool TypesOK = true;
    for (Value *V : Inputs)
      TypesOK &= boundaryType(V->getType(), DL) && !V->isSwiftError();
    for (Value *V : Outputs)
      TypesOK &= boundaryType(V->getType(), DL);
    if (!TypesOK) {
      Skip(BoundaryType);
      continue;
    }
    if (Inputs.size() > 6 || Outputs.size() > 2) {
      Skip(BoundaryBudget);
      continue;
    }
    ++Eligible;
    if (Eligible == 1) {
      Selected = &BB;
    } else {
      // Reservoir selection needs no candidate vector, even for large CFGs.
      uint64_t R;
      do {
        R = nextRandom(Seed);
      } while (R < -Eligible % Eligible);
      if (R % Eligible == 0)
        Selected = &BB;
    }
  }
  if (Stats)
    Stats->eligible = Eligible;
  if (!Selected)
    return 0;
  Skip(SelectionBudget, Eligible - 1);
  CodeExtractor CE({Selected}, &DT, false, nullptr, nullptr, nullptr, false,
                   false, nullptr, "lyra_outline");
  CodeExtractorAnalysisCache Cache(*F);
  Function *Helper = CE.extractCodeRegion(Cache);
  if (!Helper)
    return -1;
  Helper->setLinkage(GlobalValue::InternalLinkage);
  Helper->removeFnAttr(Attribute::AlwaysInline);
  Helper->removeFnAttr(Attribute::InlineHint);
  Helper->addFnAttr(Attribute::NoInline);
  // LLVM creates both helper and call with the default C convention. It does
  // not copy caller parameter/return attributes, in particular noundef,
  // noalias, signext, or zeroext. Do not infer such promises for live values:
  // poison/undef can cross this boundary without becoming immediate UB.
  return 1;
}
