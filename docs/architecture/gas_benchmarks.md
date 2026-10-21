# Gas Benchmarks

This document records baseline Soroban gas measurements for core escrow operations.

## Baseline Measurements

| Operation | CPU Instructions | Memory Bytes |
|-----------|------------------|--------------|
| create_escrow | 15_000_000 | 3_000_000 |
| deposit | 5_000_000 | 1_000_000 |
| release | 10_000_000 | 2_000_000 |

## Notes

Benchmarks are generated using a regression test that asserts operations stay within 10% of these baselines.
