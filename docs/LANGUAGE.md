# Architecture Language

Words we use consistently when arguing about how the codebase should be
shaped. Borrowed from John Ousterhout's deep-modules vocabulary.

## Module

Anything with an interface and an implementation. A crate, a struct with
public methods, a trait with adapters.

## Interface

Everything a caller must know to use the module correctly: types,
invariants, error modes, ordering guarantees, configuration, performance
expectations — not just method signatures.

## Implementation

The code behind the interface. May be elaborate; *should* be hidden.

## Depth

The ratio of leverage delivered to surface area exposed. A deep module
gives a lot behind a small interface. A shallow module exposes nearly as
much complexity as it hides.

## Leverage

How much work the caller can offload onto the module relative to how much
they must learn. `WorldRuntime::advance` is high-leverage: callers say
"advance" and get an explained delta of the entire simulation.

## Locality

Behavior that belongs together stays together. The signal-matching system,
the change log, and the explanation rendering are all in the `world`
crate because their concepts are coupled.

## Seam

A place where an interface lives — a point where behavior can be altered
without editing in place. The `EventLog` trait is a seam: the world
runtime is unaware of whether the backend is in-memory or JSONL.

## Adapter

A concrete implementation that satisfies an interface at a seam.
`MemoryEventLog`, `JsonlEventLog`, `FakeAgent` are adapters.

## Deletion test

If deleting a module makes complexity vanish, it was probably a
pass-through and never earned its keep. If deleting it makes complexity
reappear across many callers, the module was paying rent.

## When to introduce a seam

One adapter is a hypothetical seam. Two adapters is a real seam. Don't
create plugin/provider/abstraction layers because they look clean —
create them only where behavior actually varies. Today we have two
EventLog adapters (memory, jsonl) and only one AgentBridge adapter
(`FakeAgent`). The agent seam stays generic-but-thin until a second
adapter shows up.
