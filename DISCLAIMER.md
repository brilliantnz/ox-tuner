# Disclaimer & safety warning

**Ox Tuner is experimental software under active development. It does not work yet.**

This software is intended to read from, and eventually write to, an engine control
unit (ECU) that controls a combustion engine. Misuse can destroy an engine, damage
hardware, injure people, or make a vehicle illegal to operate.

## No warranty

ox-tuner is licensed under GPL-3.0-or-later and is provided **"as is", without
warranty of any kind**, express or implied, including but not limited to the
warranties of merchantability and fitness for a particular purpose. The authors and
contributors are not liable for any damage to equipment, engines, vehicles, data, or
persons resulting from its use. See [LICENSE](./LICENSE).

## Test in stages — never on a running engine first

1. **Simulator** — no hardware attached.
2. **Bench** — ECU on a desk with a stable power supply, engine not connected.
3. **Engine** — only after the bench tests pass, at your own risk.

Never run an engine while writing or burning a tune unless you have a verified,
recoverable baseline and understand the failure modes.

## Guarantees this project does not make

- A successful write **does not** mean the tune is safe, correct, or suitable for
  your engine, fuel, climate, or legal limits.
- Emissions and road-legality compliance is entirely the user's responsibility.

## Your data

Connecting to an ECU reads and writes calibration data. Keep backups of a known-good
tune before any write, the project provides no recovery guarantee.
