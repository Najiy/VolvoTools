import { useState } from "react";
import { Banner } from "./Banner";
import { Button } from "./Button";

interface StopControlProps {
  cancel: (force: boolean) => Promise<void>;
}

/**
 * Stops the running job. The backend refuses a plain stop while the ECU is being erased or
 * written; the refusal is shown here with an explicit, separate "Force stop".
 */
export function StopControl({ cancel }: StopControlProps) {
  const [refusal, setRefusal] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function stop(force: boolean) {
    setBusy(true);
    try {
      await cancel(force);
      setRefusal(null);
    } catch (error) {
      setRefusal(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }

  if (refusal) {
    return (
      <Banner
        tone="warn"
        title="Not stopped"
        actions={
          <>
            <Button size="sm" variant="quiet" onClick={() => setRefusal(null)}>
              Keep going
            </Button>
            <Button size="sm" variant="danger" busy={busy} onClick={() => stop(true)}>
              Force stop
            </Button>
          </>
        }
      >
        <p>{refusal}</p>
      </Banner>
    );
  }
  return (
    <div className="row">
      <Button icon="stop" busy={busy} onClick={() => stop(false)}>
        Stop
      </Button>
    </div>
  );
}
