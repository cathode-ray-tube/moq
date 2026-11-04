import * as Moq from "@moq/net";

const status = document.querySelector("#status")!;
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

subscribeButton.addEventListener("click", async () => {
  subscribeButton.disabled = true;
  stopped = false;

  try {
    const url = new URL(
      document.querySelector<HTMLInputElement>("#url")!.value,
    );
    const name = document
      .querySelector<HTMLInputElement>("#name")!
      .value.trim();
    const trackName = document
      .querySelector<HTMLInputElement>("#track")!
      .value.trim();

    const origin = new Moq.Origin.Producer();

    connection = await Moq.Connection.connect({
      url,
      publish: origin.consume(),
      consume: origin,
    });

    request = origin.request(Moq.Path.from(name));

    let active = request.active.peek();
    while (!active && !stopped) {
      await request.active.changed();
      active = request.active.peek();
    }

    if (stopped || !active) return;

    consumer = active.track(trackName).subscribe({ priority: 0 });
    status.textContent = `Subscribed to ${name} / ${trackName}. Check the console.`;

    for (;;) {
      const group = await consumer.recvGroup();
      if (!group || stopped) break;

      try {
        for (;;) {
          const frame = await group.readFrame();
          if (!frame || stopped) break;

          console.log("Received frame", {
            bytes: frame.payload.byteLength,
            timestamp: frame.timestamp,
            firstBytes: Array.from(frame.payload.slice(0, 12)),
          });
        }
      } catch (error) {
        console.warn("Could not finish this group; waiting for the next one", error);
      }
    }

    if (!stopped) {
      status.textContent = "Track ended.";
    }
  } catch (error) {
    console.error(error);

    if (!stopped) {
      status.textContent = `${(error as Error)?.name ?? "Error"}: ${
        (error as Error)?.message ?? error
      }`;
    }
  } finally {
    subscribeButton.disabled = false;
  }
});

window.addEventListener("beforeunload", () => {
  stopped = true;
  consumer?.close?.();
  request?.close?.();
  connection?.close?.();
});
