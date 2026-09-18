# Ultra verification

- Production TypeScript + Vite build: PASS.
- Desktop 1440x900: toggle on/off, no horizontal overflow, pixels inspected.
- Mobile 390x844: toggle on/off, no horizontal overflow, pixels inspected.
- Keyboard: Enter activates the explicit off path.
- Reduced motion: static transformed state is immediate; transition duration reports 0s.
- Refresh/local state: Ultra intentionally does not persist; localStorage only contained the existing motion preference.
- Capability truth: status reads "Premium preview engaged" and "No extra capabilities are active"; model remains not connected.
- Performance sample (headless Chromium, 2.5 seconds each): {"base":{"frames":151,"duration":2509.4000000000233,"heap":10000000},"ultra":{"frames":82,"duration":2524.0999999999767,"heap":10000000},"frameDelta":-69,"heapDelta":0}. This is a local frame/JS-heap smoke test, not OS-level production CPU/RAM telemetry.
