function validSequence(value) {
  const sequence = Number(value);
  return Number.isSafeInteger(sequence) && sequence >= 0 ? sequence : null;
}

export function createEventSequenceTracker() {
  let current = null;

  return {
    current() {
      return current;
    },

    observe(event) {
      const ready = event?.type === "bridge_event_stream" && event.status === "ready";
      const sequence = validSequence(ready ? event.sequence : event?.bridge_sequence);
      if (sequence === null) return { accept: true, gap: null };

      if (ready) {
        const previous = current;
        current = sequence;
        if (previous === null || previous === sequence) return { accept: true, gap: null };
        return {
          accept: true,
          gap: {
            type: "bridge_event_gap",
            reason: sequence < previous ? "sequence_reset" : "reconnect",
            skipped: Math.max(0, sequence - previous),
            previous_sequence: previous,
            sequence,
          },
        };
      }

      if (current !== null && sequence <= current) return { accept: false, gap: null };
      const gap =
        current !== null && sequence > current + 1
          ? {
              type: "bridge_event_gap",
              reason: "sequence_gap",
              skipped: sequence - current - 1,
              previous_sequence: current,
              sequence,
            }
          : null;
      current = sequence;
      return { accept: true, gap };
    },
  };
}
