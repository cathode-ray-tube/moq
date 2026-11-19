# @cathode-ray-tube/moq-secure-publish

A modified fork of [`@moq/publish`](https://www.npmjs.com/package/@moq/publish) that adds encryption and signing through [`@cathode-ray-tube/moq-secure`](https://www.npmjs.com/package/@cathode-ray-tube/moq-secure).

This package is designed to work with the companion package [`@cathode-ray-tube/moq-secure-watch`](https://www.npmjs.com/package/@cathode-ray-tube/moq-secure-watch). Use this package to publish a broadcast and the companion package to watch it.

## Example

This example publishes screen video. Enter a relay URL and broadcast name, then select **Start screen and publish**. Use the same relay, broadcast name, and key settings in the watch example.

Save as `index.html` and serve it from a local web server.

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>MoQ publisher</title>
  </head>
  <body>
    <h1>MoQ publisher</h1>

    <label>
      Relay URL
      <input
        id="url"
        type="url"
        placeholder="https://your-relay.example"
        size="45"
      />
    </label>

    <label>
      Broadcast name
      <input id="name" value="me.hang" />
    </label>

    <button id="start">Start screen and publish</button>
    <button id="stop" disabled>Stop</button>
    <pre id="status">Not started</pre>

    <script type="module">
      import * as Publish from "https://esm.sh/@cathode-ray-tube/moq-secure-publish@0.1.0";
      import {
        InMemoryKeyStore,
        MoqSecureEncrypter,
      } from "https://esm.sh/@cathode-ray-tube/moq-secure@0.1.10";

      const status = document.querySelector("#status");
      const startButton = document.querySelector("#start");
      const stopButton = document.querySelector("#stop");

      let connection;
      let origin;
      let broadcast;
      let screen;
      let capture;
      let encoder;

      startButton.addEventListener("click", async () => {
        startButton.disabled = true;

        try {
          const urlText = document.querySelector("#url").value.trim();
          if (!urlText) throw new Error("Enter a relay URL.");

          const url = new URL(urlText);
          const name = document.querySelector("#name").value.trim();

          if (!name) throw new Error("Enter a broadcast name.");

          // Create the screen source in the click handler so the browser
          // can request screen-capture permission from a user gesture.
          screen = new Publish.Source.Screen({ enabled: true });
          origin = new Publish.Net.Origin.Producer();

          connection = await Publish.Net.Connection.connect({
            url,
            publish: origin.consume(),
            consume: origin,
          });

          broadcast = new Publish.Broadcast({
            origin,
            enabled: true,
            name: Publish.Net.Path.from(name),
          });

          const video = new Publish.Signals.Computed(
            (effect) => effect.get(screen.out.source)?.video,
          );

          capture = new Publish.Video.Capture({ source: video });

          // TEST KEY ONLY. Replace with a securely generated 32-byte key.
          const keyStore = new InMemoryKeyStore();
          keyStore.setKeyEncoded(
            1,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
          );

          const encrypter = new MoqSecureEncrypter({
            keyStore,
            signingPrivateKey: new Uint8Array(),
            keyId: 1,
            nSigned: 0,
            padLen: 0,
          });

          encoder = new Publish.Video.Encoder("video/hd", {
            broadcast,
            capture,
            enabled: true,
            encrypter,
          });

          stopButton.disabled = false;
          status.textContent =
            `Publishing ${name}\nTrack: video/hd\nWaiting for a subscriber…`;
        } catch (error) {
          console.error(error);
          status.textContent =
            `${error?.name ?? "Error"}: ${error?.message ?? error}`;

          encoder?.close();
          capture?.close();
          screen?.close();
          connection?.close();

          encoder = undefined;
          capture = undefined;
          screen = undefined;
          connection = undefined;
          broadcast = undefined;
          origin = undefined;

          startButton.disabled = false;
        }
      });

      stopButton.addEventListener("click", () => {
        encoder?.enabled.set(false);
        screen?.enabled.set(false);
        connection?.close();

        encoder = undefined;
        capture = undefined;
        screen = undefined;
        connection = undefined;
        broadcast = undefined;
        origin = undefined;

        stopButton.disabled = true;
        startButton.disabled = false;
        status.textContent = "Stopped";
      });
    </script>
  </body>
</html>
```

The key shown is a test value only. Use a securely generated key for real broadcasts, and configure the viewer in `@cathode-ray-tube/moq-secure-watch` with the matching key ID and key bytes. This example publishes video; audio publishing is not currently supported.
