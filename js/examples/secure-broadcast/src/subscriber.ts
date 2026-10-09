import * as Moq from "@moq/net";

const status = document.querySelector("#status");
const subscribeButton = document.querySelector("#subscribe");

let connection;
let request;
let consumer;
let stopped = false;

subscribeButton.addEventListener("click", async () => {
  subscribeButton.disabled = true;
  stopped = false;

  try {
    const url = new URL(document.querySelector("#url").value);
    const name = document.querySelector("#name").value.trim();
    const trackName = document.querySelector("#track").value.trim();

    connection = new Moq.Connection({ url });

    // Ask the connected origin for the named broadcast.
    request = connection.origin.request(Moq.Path.from(name));

    // Wait until the broadcast is available.
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

      for (;;) {
        const frame = await group.readFrame();
        if (!frame || stopped) break;

        // Log metadata and a short byte prefix, not the entire video payload.
        console.log("Received frame", {
          bytes: frame.payload.byteLength,
          timestamp: frame.timestamp,
          firstBytes: Array.from(frame.payload.slice(0, 12)),
        });
      }
    }

    status.textContent = "Track ended.";
  } catch (error) {
    console.error(error);
    status.textContent = `${error?.name ?? "Error"}: ${error?.message ?? error}`;
    subscribeButton.disabled = false;
  }
});

window.addEventListener("beforeunload", () => {
  stopped = true;
  consumer?.close?.();
  request?.close?.();
  connection?.close?.();
});
