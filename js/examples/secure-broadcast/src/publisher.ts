import * as Publish from "@moq/publish";
import {
  InMemoryKeyStore,
  MoqSecureEncrypter,
} from "moq-secure";

const status = document.querySelector("#status")!;
const startButton = document.querySelector<HTMLButtonElement>("#start")!;
const stopButton = document.querySelector<HTMLButtonElement>("#stop")!;

let connection: Publish.Net.Connection | undefined;
let origin: Publish.Net.Origin.Producer | undefined;
let broadcast: Publish.Broadcast | undefined;
let camera: InstanceType<typeof Publish.Source.Camera> | undefined;
let encoder: Publish.Video.Encoder | undefined;

startButton.addEventListener("click", async () => {
  startButton.disabled = true;

  try {
    const url = new URL(
      document.querySelector<HTMLInputElement>("#url")!.value,
    );
    const name = document
      .querySelector<HTMLInputElement>("#name")!
      .value.trim();

    if (!name) throw new Error("Enter a broadcast name.");

    // Create the camera in the click handler so permission is requested
    // from a user gesture.
    camera = new Publish.Source.Camera({ enabled: true });

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
      (effect) => effect.get(camera!.out.source)?.video,
    );
    const capture = new Publish.Video.Capture({ source: video });

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
      `${(error as Error)?.name ?? "Error"}: ${(error as Error)?.message ?? error}`;
    startButton.disabled = false;
  }
});

stopButton.addEventListener("click", () => {
  encoder?.enabled.set(false);
  camera?.enabled.set(false);
  connection?.close();

  encoder = undefined;
  broadcast = undefined;
  camera = undefined;
  connection = undefined;
  origin = undefined;

  stopButton.disabled = true;
  startButton.disabled = false;
  status.textContent = "Stopped";
});
