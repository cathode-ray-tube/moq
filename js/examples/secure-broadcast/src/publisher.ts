import * as Publish from "@moq/publish";
import {
  InMemoryKeyStore,
  MoqSecureEncrypter,
} from "moq-secure";

const status = document.querySelector("#status");
const startButton = document.querySelector("#start");
const stopButton = document.querySelector("#stop");

let connection;
let broadcast;
let camera;
let encoder;

startButton.addEventListener("click", async () => {
  startButton.disabled = true;

  try {
    const url = new URL(document.querySelector("#url").value);
    const name = document.querySelector("#name").value.trim();

    if (!name) throw new Error("Enter a broadcast name.");

    // Construct the camera source in this click handler so its permission
    // request happens as a result of the user gesture.
    camera = new Publish.Source.Camera({ enabled: true });

    connection = new Publish.Net.Connection({
      url,
      enabled: true,
    });

    broadcast = new Publish.Broadcast({
      origin: connection.origin,
      enabled: true,
      name: Publish.Net.Path.from(name),
    });

    const video = new Publish.Signals.Computed(
      (effect) => effect.get(camera.out.source)?.video,
    );
    const capture = new Publish.Video.Capture({ source: video });

    // TEST KEY ONLY. Replace it with a securely generated 32-byte key.
    // The receiver must use the same key and keyId to decrypt later.
    const keyStore = new InMemoryKeyStore();
    keyStore.setKeyEncoded(
      1,
      "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    );

    // Signing is disabled here. Check encryptFrame to confirm whether it
    // accepts an empty signingPrivateKey when nSigned is 0.
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
    status.textContent = `${error?.name ?? "Error"}: ${error?.message ?? error}`;
    startButton.disabled = false;
  }
});

stopButton.addEventListener("click", () => {
  // Disabling components lets their effects clean up.
  encoder?.enabled?.set?.(false);
  camera?.enabled?.set?.(false);

  connection?.close?.();

  encoder = undefined;
  camera = undefined;
  broadcast = undefined;
  connection = undefined;

  stopButton.disabled = true;
  startButton.disabled = false;
  status.textContent = "Stopped";
});
