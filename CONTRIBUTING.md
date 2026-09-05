# Contributing to Delego Contracts

Thank you for your interest in contributing to Delego! This repository hosts the Soroban smart contracts that anchor trust-critical state for the Delego platform. We welcome contributions from everyone and are excited to have you join our community.

## Table of Contents

- [Getting Started](#getting-started)
- [Development Workflow](#development-workflow)
- [Code Standards](#code-standards)
- [Project Layout](#project-layout)
- [Testing Guidelines](#testing-guidelines)
- [Pull Request Process](#pull-request-process)
- [Reporting Issues](#reporting-issues)
- [Security](#security)
- [Contact](#contact)

## Getting Started

### Prerequisites

Before you begin contributing, ensure you have the following installed:

- **Rust** >= 1.70 (with `rustfmt` and `clippy` components)
- **WASM target** for building contract artifacts:
  ```bash
  rustup target add wasm32-unknown-unknown
  ```

### Setup Instructions

1. **Fork the Repository**
   ```bash
   # Fork the repository on GitHub
   # Then clone your fork
   git clone https://github.com/YOUR_USERNAME/Delego-contracts.git
   cd delego-contracts
   ```

2. **Verify the workspace builds**
   ```bash
   cargo build --workspace
   ```

3. **Run the test suite**
   ```bash
   cargo test --workspace
   ```

The workspace contains four crates: `delego-escrow`, `delego-permissions`, `delego-delegation-registry`, and `delego-cross-contract-tests`.

## Development Workflow

### 1. Choose an Issue

- Browse [GitHub Issues](https://github.com/DelegoLabs/Delego-contracts/issues) for open issues
- Look for issues labeled `good first issue` if you're new to the project
- Comment on the issue to claim it and ask questions if needed
- Create a new issue if you've found a bug or have a feature request

### 2. Create a Branch

```bash
# Ensure your main branch is up to date
git checkout main
git pull upstream main

# Create a feature branch
git checkout -b feat/your-feature-name
# or
git checkout -b fix/your-bug-fix
```

**Branch Naming Convention:**
- `feat/` - New features
- `fix/` - Bug fixes
- `docs/` - Documentation changes
- `refactor/` - Code refactoring
- `test/` - Test additions or changes
- `chore/` - Maintenance tasks

### 3. Make Your Changes

- Write clear, focused commits
- Follow the [Conventional Commits](https://www.conventionalcommits.org/) specification
- Add tests for new functionality (see [Testing Guidelines](#testing-guidelines))
- Update documentation as needed

**Commit Message Format:**
```
type(scope): subject

body

footer
```

Examples:
```
feat(escrow): add partial_release for staged payouts

Allow partial release of escrowed funds to the seller across multiple
deliveries, respecting the buyer's overall release authority.

Closes #123
```

### 4. Verify Your Changes

```bash
# Format the code
cargo fmt --all -- --check

# Run clippy (must be warning-free)
cargo clippy --workspace --all-targets -- -D warnings

# Run the full test suite
cargo test --workspace

# Build WASM artifacts (required for contract changes)
cargo build --target wasm32-unknown-unknown --release
```

These checks run in CI, so passing them locally first speeds up review.

### 5. Submit a Pull Request

- Push your branch to your fork
- Open a pull request against the `main` branch
- Use the PR template and provide a detailed description
- Link related issues
- Request review from maintainers

## Code Standards

### Rust (Soroban Contracts)

- **Error Handling**: Use typed `Result` return values with contract-specific error enums (`EscrowError`, `PermissionError`, …)
- **Authentication**: Use Soroban `Address` `require_auth()` / `require_auth_for_args()` — never trust the caller address implicitly
- **Testing**: Include comprehensive tests for every public function (see below)
- **Documentation**: Document public functions and structs with doc comments
- **Events**: Emit events for state transitions that off-chain services must observe
- **Gas Efficiency**: Prefer compact storage, `symbol_short!` topics, and checked arithmetic
- **Safety**: No `unwrap()` in contract logic without a fallible alternative; use `overflow-checks = true` (already set in the release profile)

```rust
// Good
pub fn escrow_funds(env: Env, amount: i128) -> Result<(), EscrowError> {
    if amount <= 0 {
        return Err(EscrowError::InvalidAmount);
    }
    // Implementation
    Ok(())
}

// Bad
pub fn escrow_funds(env: Env, amount: i128) {
    // Implementation without error handling
}
```

### General Guidelines

- **TODO Comments**: Mark incomplete logic with `// TODO:` and link to an issue when possible
- **Code Comments**: Add comments for complex logic, not obvious code
- **Function Length**: Keep functions focused and reasonably short
- **Imports**: Organize imports logically (stdlib, external, internal)

### no_std Policy

All contract crates (`delegation_registry`, `escrow`, `permissions`, `reputation`, `marketplace`) must declare `no_std` using the exact conditional attribute form:

```rust
// Contract crates compile as no_std for release and wasm builds, but keep std
// enabled during testing so dev-dependencies and test assertions operate normally.
// This exact conditional form must be consistent across all workspace contract crates.
#![cfg_attr(not(test), no_std)]
```

#### Rationale & Consistency
- **Why conditional?** Contracts compile as `no_std` for release and `wasm32-unknown-unknown` deployment targets, but test suites and dev-dependencies (such as cryptographic test helpers or `ed25519-dalek`) require the Rust standard library (`std`) when executing under `cargo test`.
- **Why consistency matters:** `cargo test` links `std` regardless of whether unconditional `#![no_std]` or conditional `#![cfg_attr(not(test), no_std)]` is declared. If attributes drift across crates, a contract using unconditional `#![no_std]` can pass `cargo test` while its actual `wasm32-unknown-unknown` build fails. Enforcing this exact attribute across all contract crates guarantees uniform build and testing behavior across the workspace.


## Project Layout

```
delego-contracts/
├── escrow/                 # Escrow contract (delego-escrow)
├── permissions/            # Permissions contract (delego-permissions)
├── delegation_registry/    # Delegation registry contract (delego-delegation-registry)
├── tests/                  # Cross-contract integration tests (delego-cross-contract-tests)
├── docs/architecture/      # Architecture documentation
└── Cargo.toml              # Workspace manifest
```

Related work lives in sibling repositories:

| Repository | Purpose |
|---|---|
| [Delego](https://github.com/DelegoLabs/Delego) | Frontend web application |
| [Delego-backend](https://github.com/DelegoLabs/Delego-backend) | Backend microservices, agents, shared SDK/types |

## Testing Guidelines

### Test Structure

Each contract keeps its tests as modules alongside the source:

- `src/test.rs` — `#[cfg(test)]` unit tests
- `src/integration_tests.rs` — contract-level integration tests
- `tests/cross_contract.rs` — cross-contract integration tests (root `tests/` package)

### Test Coverage

- Every public function should have at least one happy-path and one failure-path test
- Test auth requirements (unauthenticated calls must fail)
- Test limit boundaries (zero, at-limit, over-limit amounts)
- Test idempotency for operations that must be safe to replay
- Test event emission for state transitions

### Running Tests

```bash
# Run all workspace tests (unit + integration + cross-contract)
cargo test --workspace

# Run tests with output
cargo test -- --nocapture

# Run a single test
cargo test test_function_name

# Run tests for one contract
cargo test -p delego-escrow
```

## Pull Request Process

### Before Submitting

1. **Code Quality**
   - [ ] `cargo fmt --all -- --check` passes
   - [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
   - [ ] `cargo test --workspace` passes

2. **Testing**
   - [ ] Tests added for new functionality
   - [ ] All tests pass
   - [ ] Event emission covered where state changes

3. **Documentation**
   - [ ] Contract README updated if adding/changing public functions
   - [ ] `docs/architecture/contracts.md` updated if changing contract design
   - [ ] Comments added for complex logic

4. **Commit Messages**
   - [ ] Follows Conventional Commits specification
   - [ ] Clear and descriptive
   - [ ] Links to related issues

### Submitting the PR

1. **Title**: Use a clear, descriptive title following Conventional Commits
2. **Description**: Provide a detailed description of changes
3. **Related Issues**: Link to related issues using `Closes #123` or `Fixes #123`
4. **Checklist**: Complete the PR template checklist

### Review Process

- Maintainers will review your PR
- Address feedback in a timely manner
- Be open to suggestions and improvements
- Keep discussions focused and constructive

### After Merge

- Delete your feature branch
- Celebrate your contribution! 🎉

## Reporting Issues

### Bug Reports

When reporting a bug, include:

1. **Clear Title**: Descriptive title for the issue
2. **Description**: Detailed description of the problem
3. **Reproduction Steps**: Steps to reproduce the issue
4. **Expected Behavior**: What you expected to happen
5. **Actual Behavior**: What actually happened
6. **Environment Details**:
   - OS: [e.g., macOS, Ubuntu, Windows]
   - Rust version: `rustc --version`
   - Contract/revision: [e.g., escrow @ main]

**Example:**
```
Title: Escrow release can be called before delivery timeout

Description:
The release path does not enforce the delivery timeout when called by the
buyer, allowing funds to be released before the agreed settlement window.

Steps to Reproduce:
1. Create and fund an escrow
2. Call release immediately after funding
3. Observe funds released despite the timeout

Expected Behavior:
Release should be rejected until the timeout ledger elapses.

Actual Behavior:
Release succeeds immediately.

Environment:
- OS: Ubuntu 22.04
- Rust: 1.85
- Commit: <sha>
```

### Feature Requests

When requesting a feature, include:

1. **Clear Title**: Descriptive title for the feature
2. **Description**: Detailed description of the feature
3. **Use Case**: Why this feature is needed
4. **Proposed Solution**: How you envision the feature working
5. **Alternatives**: Any alternative solutions considered
6. **Additional Context**: Any other relevant information

## Security

### Reporting Security Vulnerabilities

**Do not** open public issues for security vulnerabilities.

To report a security vulnerability:

1. Email us at: security@delego.dev
2. Include details and reproduction steps
3. We will respond promptly and coordinate disclosure
4. We will work with you to fix the issue
5. We will coordinate the public disclosure timeline

### Security Best Practices

- Never commit secrets or API keys
- Treat all on-chain input as untrusted
- Review auth flows thoroughly (`require_auth`)
- Consider reentrancy, overflow, and rounding edge cases
- Test security-related functionality thoroughly

## Community Guidelines

### Code of Conduct

Please read and follow our [Code of Conduct](./CODE_OF_CONDUCT.md).

### Communication

- Be respectful and constructive in all communications
- Welcome newcomers and help them get started
- Focus on what is best for the community
- Show empathy towards other community members

### Getting Help

- Check existing documentation first
- Search GitHub Issues for similar problems
- Ask questions in GitHub Discussions
- Join our community chat (link coming soon)

## Contact

- **GitHub Issues**: For bugs and feature requests
- **GitHub Discussions**: For questions and general discussion
- **Security**: security@delego.dev (for security issues only)

## Thank You

Thank you for contributing to Delego! Your contributions help make AI-powered delegated commerce more accessible and secure for everyone.

For more detailed information, see the [repository README](./README.md) and [architecture documentation](./docs/architecture/contracts.md).
