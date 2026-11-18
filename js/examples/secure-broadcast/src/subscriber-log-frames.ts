import * as Moq from "@moq/net";

const status = document.querySelector<HTMLElement>("#status")!;
const subscribeButton =
  document.querySelector<HTMLButtonElement>("#subscribe")!;

let connection: Moq.Connection | undefined;
let request: ReturnType<Moq.Origin.Producer["request"]> | undefined;
let consumer: ReturnType<
  ReturnType<Moq.Origin.Producer["request"]>["active"]["peek"] extends () => infer T
    ? NonNullable<T>["track"]
    : never
> | undefined;

let stopped = false;

function closeCurrentSubscription() {
  stopped = true;
  consumer?.close?.();
  consumer = undefined;

  request?.close?.();
  request = undefined;

  connection?.close?.();
  connection = undefined;
}

subscribeButton.addEventListener("click", async () => {
  closeCurrentSubscription();
  stopped = false;
  subscribeButton.disabled = true;

  try {
    const url = new URL(
      document.querySelector<HTMLInputElement>("#url")!.value.trim(),
    );
    const name = document
      .querySelector<HTMLInputElement>("#name")!
      .value.trim();
    const trackName = document
      .querySelector<HTMLInputElement>("#track")!
      .value.trim();

    if (!name) throw new Error("Enter a broadcast name.");
    if (!trackName) throw new Error("Enter a track name.");

    console.info("[moq] connecting", {
      url: `${url.origin}${url.pathname}`,
      name,
      trackName,
      at: new Date().toISOString(),
    });

    const origin = new Moq.Origin.Producer();

    connection = await Moq.Connection.connect({
      url,
      publish: origin.consume(),
      consume: origin,
    });

    if (stopped) return;

    console.info("[moq] connected; requesting broadcast", { name });
    request = origin.request(Moq.Path.from(name));

    let active = request.active.peek();

    while (!active && !stopped) {
      await request.active.changed();
      active = request.active.peek();
    }

    if (stopped || !active) return;

    console.info("[moq] broadcast active; subscribing", { name, trackName });
    consumer = active.track(trackName).subscribe({ priority: 0 });

    status.textContent =
      `Subscribed to ${name} / ${trackName}. Check the console for frames.`;

    let frameCount = 0;

    while (!stopped) {
      const group = await consumer.recvGroup();
      if (!group || stopped) break;

      try {
        while (!stopped) {
          const frame = await group.readFrame();
          if (!frame || stopped) break;

          frameCount++;

          console.log("[moq] received frame", {
            frameCount,
            bytes: frame.payload.byteLength,
            timestamp: frame.timestamp,
            firstBytes: Array.from(frame.payload.slice(0, 12)),
          });
        }
      } catch (error) {
        if (!stopped) {
          console.warn(
            "[moq] Could not finish this group; waiting for the next one",
            error,
          );
        }
      }
    }

    if (!stopped) {
      status.textContent = `Track ended. Received ${frameCount} frames.`;
      console.info("[moq] track ended", { frameCount });
    }
  } catch (error) {
    if (!stopped) {
      console.error("[moq] subscriber error", error);
      status.textContent =
        `${(error as Error)?.name ?? "Error"}: ` +
        `${(error as Error)?.message ?? error}`;
    }
  } finally {
    subscribeButton.disabled = false;
  }
});

window.addEventListener("beforeunload", () => {
  closeCurrentSubscription();
});

