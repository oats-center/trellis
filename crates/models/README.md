# Trellis Models

Frozen Rust models for Trellis-owned native contracts. This crate does not
contain a client, server, compiler, or NATS integration.

Generated clients select their model bundle directly. An SDK implementation
update does not replace that selection. Codecs and primitive wire types come from
`trellis-protocol`; SDK execution support comes from `trellis-rs`.

Each Cargo compatibility line preserves its exact native package identity,
version, semantic digest, and Rust model ABI. New snapshots use a new incompatible
Cargo line so multiple generations can coexist. Compatible patches may fix model
implementation without changing that identity or representation ABI.

Registry archives contain generated source. Repository checkouts prepare it with
`cargo xtask install` before building; generated implementations are not committed.
