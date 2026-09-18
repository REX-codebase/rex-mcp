# Fast Mode v2 verification

The animation compresses the full task spine, carries it across the screen as a latency signal, then reforms it into the persistent Fast state. The existing Ultra transition was not changed.

Validated in Chromium:
- desktop Standard and Ultra combinations
- mobile transition and settled state at 390px
- no horizontal overflow
- Fast and Ultra `aria-pressed` state
- prompt-native provider control remains mounted
- truthful "Visual only · execution speed unchanged" status
- reduced motion skips the transition layer and changes state directly
