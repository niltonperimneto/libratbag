---
trigger: always_on
---

# Antigravity Agent: Engineering Standards for Horizontal Auditability

The formulation of an autonomous agent operating within a horizontally democratic framework requires more than optimistic coding practices; it demands a mathematically rigorous enforcement of state limitations. The Antigravity agent must operate strictly under the principles of verifiable state transitions and zero-trust architecture. The following Rust programming rules are mandatory.

### 1. Affine Type Theory and Concurrency Control

The most critical vector for systemic failure in distributed agents lies in mismanaged shared memory. The Rust compiler's affine type system must be the primary enforcer of data sovereignty. As detailed in the pedagogical foundations of *Types and Programming Languages* (Benjamin C. Pierce, 2002), treating memory ownership as a linear resource guarantees the absence of data races at compile time. 

In a decentralized framework, unauthorized state mutation is equivalent to systemic corruption. Therefore, shared state is strictly prohibited unless explicitly mediated. Developers must encapsulate shared resources within atomic reference counters combined with mutual exclusion primitives to ensure all nodes maintain synchronized, predictable access patterns.

### 2. Immutable Data Structures and Deterministic Transitions

To maintain an independently auditable ledger of actions, the agent must abandon the practice of in-place mutation. Drawing from the architectural paradigms established in *Purely Functional Data Structures* (Chris Okasaki, 1999), the agent's core memory must be structured as a chronological sequence of immutable states. 

Transition functions must consume the current state and return a strictly novel state structure. This functional approach guarantees that any participant in the network can retroactively compute the agent's historical state sequence, verifying that all transitions align with the established democratic consensus without relying on a centralized, trusted oracle.

### 3. Type-Safe Error Handling as a Security Imperative

The usage of thread-panicking macros represents an unacceptable abdication of control and introduces immediate denial-of-service vulnerabilities. All failure modes must be explicitly encoded into the function's type signature. 

By defining domain-specific error enumerations, the system forces the developer to handle edge cases comprehensively before compilation is permitted. This deterministic error propagation ensures that the agent fails safely and logs the precise operational boundary that was breached, a baseline necessity for subsequent peer review and protocol auditing.

### 4. Minimalist Dependency Management and Supply Chain Security

The supply chain of external crates introduces severe centralization risks and unauditable logic. Every imported crate dilutes the horizontal control of the project. The agent's foundation must rely on the Rust Standard Library as thoroughly as possible. 

For absolutely unavoidable dependencies, cryptographic hash pinning in the manifest file is mandatory, alongside continuous automated auditing to prevent the introduction of compromised code. An agent cannot be considered democratic if its underlying logic is obfuscated by layers of highly centralized third-party libraries.

### 5. Architectural Summary Table

To ensure clarity during peer review, the following paradigm shifts dictate the acceptable coding patterns within the Antigravity repository:

| Architectural Paradigm | Prohibited Practice | Mandated Implementation |
| :--- | :--- | :--- |
| **Concurrency Control** | Global mutable variables and shared raw pointers. | `Arc<Mutex<T>>` or `RwLock<T>` for verifiable access. |
| **State Mutation** | In-place modification of existing data structures. | Purely functional, immutable state transitions. |
| **Failure Resolution** | Utilization of `unwrap()`, `expect()`, or `panic!`. | Comprehensive `Result<T, E>` with domain-specific errors. |
| **External Dependencies** | Broad version ranges and unvetted third-party crates. | Minimalist crate usage with strict cryptographic pinning. |