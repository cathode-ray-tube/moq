import * as Moq from "@moq/net";
import { Time } from "@moq/net";

export type { BufferedRange, BufferedRanges, Frame } from "./types";

import type { AudioConfig, VideoConfig } from "../catalog";
import type { Format as ContainerFormat } from "./format";
import type { Frame } from "./types";
import type { FrameDecrypter } from "../secure/decrypter.js";
import type { FrameEncrypter } from "../secure/encrypter.js";

/** The legacy hang container: a microsecond timestamp varint followed by the raw codec payload. */
export class Format implements ContainerFormat {
	/** Configure the format for the track's media kind. */
	readonly kind: "audio" | "video" | "data";

	#decrypter?: FrameDecrypter;

	/** Configure the format from a catalog entry or an explicit media kind. */
	constructor(config: AudioConfig | VideoConfig | "audio" | "video" | "data") {
		this.kind =
			typeof config === "string"
				? config
				: "sampleRate" in config
					? "audio"
					: "video";
	}

	/**
	 * Configure this format to decrypt complete legacy-container payloads
	 * before decoding them.
	 */
	withDecrypter(decrypter: FrameDecrypter): this {
		this.#decrypter = decrypter;
		return this;
	}

	/** Write the final video frame's end timestamp before the group closes. */
	finishGroup(group: Moq.Group.Producer, end?: Time.Micro) {
		if (this.kind !== "video" || end === undefined) return;

		group.writeFrame({
			payload: encodeFrame(new Uint8Array(), end),
			timestamp: Time.Timestamp.fromMicros(end),
		});
	}

	/** Return the video-frame or audio-source endpoint for an empty codec payload. */
	end(frame: Frame): Time.Micro | undefined {
		return this.kind !== "data" && frame.payload.byteLength === 0
			? frame.timestamp
			: undefined;
	}

	/**
	 * Decode one legacy frame, including an empty-payload duration marker.
	 *
	 * Without a decrypter, this remains synchronous. With a decrypter,
	 * the returned promise resolves after decryption and authentication.
	 */
	decode(frame: Uint8Array): Frame[] | Promise<Frame[]> {
		if (!this.#decrypter) {
			return this.#decodePlaintext(frame);
		}

		return this.#decodeEncrypted(frame);
	}

	#decodePlaintext(frame: Uint8Array): Frame[] {
		const [timestamp, data] = Moq.Varint.decode(frame);

		return [{
			payload: data,
			timestamp: timestamp as Time.Micro,
			keyframe: false,
		}];
	}

	async #decodeEncrypted(frame: Uint8Array): Promise<Frame[]> {
		const plaintext = await this.#decrypter!.decrypt(frame);
		return this.#decodePlaintext(plaintext);
	}
}

/** A byte source that can be copied into a buffer, e.g. a WebCodecs EncodedChunk. */
export interface Source {
	/** Number of bytes the source will copy. */
	byteLength: number;

	/** Copy the source bytes into the given buffer. */
	copyTo(buffer: Uint8Array): void;
}

/** Encode a frame as a timestamp varint followed by the payload bytes. */
export function encodeFrame(
	source: Uint8Array | Source,
	timestamp: Time.Micro,
): Uint8Array {
	const timestampBytes = Moq.Varint.encode(timestamp);
	const data = new Uint8Array(
		timestampBytes.byteLength + source.byteLength,
	);

	data.set(timestampBytes, 0);

	if (source instanceof Uint8Array) {
		data.set(source, timestampBytes.byteLength);
	} else {
		source.copyTo(data.subarray(timestampBytes.byteLength));
	}

	return data;
}

/** Writes legacy-container frames into a MoQ track, starting a new group on each keyframe. */
export class Producer {
	#track: Moq.Track.Producer;
	#format: Format;
	#previous?: Time.Micro;
	#reordered = false;
	#group?: Moq.Group.Producer;
	#encrypter?: FrameEncrypter;

	// The newest timestamp written in the current group, which estimates its end.
	#end?: Time.Micro;

	// Furthest timestamp in finished groups, used only for discontinuity markers.
	#liveEdge?: Time.Micro;

	// The current group start and the previous group start, which bounds its frames.
	#start?: Time.Micro;
	#floor?: Time.Micro;

	// Gap between consecutive timestamps, used to close the last group when no successor exists.
	#interval?: Time.Micro;

	// A discontinuity's marker is the newest group, so another one would say nothing new.
	#marked = false;

	/** Wrap a track to publish legacy-container frames into it. */
	constructor(track: Moq.Track.Producer, format: Format) {
		this.#format = format;
		this.#track = track;
	}

	/**
	 * Configure this producer to encrypt complete legacy-container payloads
	 * before writing them to the MoQ track.
	 */
	withEncrypter(encrypter: FrameEncrypter): this {
		this.#encrypter = encrypter;
		return this;
	}

	/**
	 * Encode and append a frame; a keyframe starts a new group.
	 * Throws if the first frame is not a keyframe, a group start goes backwards,
	 * or a frame is below the previous group start.
	 */
	encode(
		data: Uint8Array | Source,
		timestamp: Time.Micro,
		keyframe: boolean,
	): void | Promise<void> {
		const floor = keyframe ? this.#start : this.#floor;

		if (floor !== undefined && timestamp < floor) {
			throw new Error("frame timestamp is below the previous group start");
		}

		this.#marked = false;

		if (!this.#encrypter) {
			this.#encodePlaintext(data, timestamp, keyframe);
			return;
		}

		return this.#encodeEncrypted(data, timestamp, keyframe);
	}

	#encodePlaintext(
		data: Uint8Array | Source,
		timestamp: Time.Micro,
		keyframe: boolean,
	): void {
		if (keyframe) {
			const rewound =
				this.#previous !== undefined &&
				timestamp < this.#previous;

			this.#close(rewound ? undefined : timestamp);

			if (rewound) this.#interval = undefined;

			this.#group = this.#track.appendGroup();
			this.#floor = this.#start;
			this.#start = timestamp;
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		}

		this.#group!.writeFrame({
			payload: encodeFrame(data, timestamp),
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});

		this.#recordFrame(timestamp);
	}

	async #encodeEncrypted(
		data: Uint8Array | Source,
		timestamp: Time.Micro,
		keyframe: boolean,
	): Promise<void> {
		if (keyframe) {
			const rewound =
				this.#previous !== undefined &&
				timestamp < this.#previous;

			await this.#closeEncrypted(rewound ? undefined : timestamp);

			if (rewound) this.#interval = undefined;

			this.#refuse(timestamp);
			this.#group = this.#track.appendGroup();
			this.#floor = this.#start;
			this.#start = timestamp;
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		} else {
			this.#refuse(timestamp);
		}

		const payload = await this.#encodeEncryptedFrame(data, timestamp);

		this.#group!.writeFrame({
			payload,
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});

		this.#recordFrame(timestamp);
	}

	/** Encode the complete legacy payload first, then encrypt it. */
	async #encodeEncryptedFrame(
		data: Uint8Array | Source,
		timestamp: Time.Micro,
	): Promise<Uint8Array> {
		return this.#encrypter!.encrypt(encodeFrame(data, timestamp));
	}

	#recordFrame(timestamp: Time.Micro): void {
		this.#reordered ||=
			this.#previous !== undefined &&
			timestamp < this.#previous;

		if (
			this.#previous !== undefined &&
			timestamp > this.#previous
		) {
			this.#interval = (timestamp - this.#previous) as Time.Micro;
		}

		this.#previous = timestamp;

		if (this.#end === undefined || timestamp > this.#end) {
			this.#end = timestamp;
		}
	}

	/**
	 * Close the current group and mark a break in the timeline: whatever comes next does not
	 * continue it. Call it when the timeline is about to jump, e.g. an encoder pausing for lack of
	 * demand or switching source; the next keyframe already rolls the group over on its own.
	 *
	 * `end` is where the content stops. Without one the group closes with no end estimated from the
	 * frame cadence, since whatever resumes may land sooner than one frame later and an end past it
	 * reads as a rewind. After closing the group, this publishes a marker group of one empty frame at
	 * `end`, or at the live edge without one. Without the marker, a group's reach runs to its
	 * successor's first frame, so the group before a pause reads as live until whatever resumes it,
	 * and a subscriber joining mid-break is handed that stale media. The marker bounds it, and it
	 * is the latest group a joiner lands on. Data tracks only close the group, since an empty payload
	 * is data. No marker is written until a frame follows the last one. Throws if `end` precedes the
	 * last video frame.
	 */
	discontinuity(end?: Time.Micro): void | Promise<void> {
		if (!this.#encrypter) {
			this.#discontinuityPlaintext(end);
			return;
		}

		return this.#discontinuityEncrypted(end);
	}

	/** @deprecated Use discontinuity() instead. */
	cut(end?: Time.Micro): void | Promise<void> {
		return this.discontinuity(end);
	}

	#discontinuityPlaintext(end?: Time.Micro): void {
		// Nothing is measured across the break, so a missing end has no cadence to estimate from.
		// An explicit end keeps the cadence until #close validates it, in case it throws.
		if (end === undefined) this.#interval = undefined;

		this.#close(end);
		this.#interval = undefined;

		this.#writeMarker(end ?? this.#liveEdge);
	}

	async #discontinuityEncrypted(end?: Time.Micro): Promise<void> {
		if (end === undefined) this.#interval = undefined;

		await this.#closeEncrypted(end);
		this.#interval = undefined;

		const timestamp = end ?? this.#liveEdge;

		if (
			this.#format.kind === "data" ||
			this.#marked ||
			timestamp === undefined
		) {
			return;
		}

		const group = this.#track.appendGroup();
		const payload = await this.#encrypter!.encrypt(
			encodeFrame(new Uint8Array(), timestamp),
		);

		group.writeFrame({
			payload,
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});
		group.close();

		this.#updateLiveEdge(timestamp);
		this.#marked = true;
	}

	#writeMarker(timestamp?: Time.Micro): void {
		if (
			this.#format.kind === "data" ||
			this.#marked ||
			timestamp === undefined
		) {
			return;
		}

		const group = this.#track.appendGroup();

		group.writeFrame({
			payload: encodeFrame(new Uint8Array(), timestamp),
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});
		group.close();

		this.#updateLiveEdge(timestamp);
		this.#marked = true;
	}

	// Flush and close the current group at the supplied or estimated end timestamp.
	#close(end?: Time.Micro): void {
		if (!this.#group) return;

		this.#validateEnd(end);

		end ??= this.#estimatedEnd();

		this.#format.finishGroup(
			this.#group,
			this.#reordered ? undefined : end,
		);

		this.#closeGroup();
	}

	async #closeEncrypted(end?: Time.Micro): Promise<void> {
		if (!this.#group) return;

		this.#validateEnd(end);

		end ??= this.#estimatedEnd();

		if (
			this.#format.kind === "video" &&
			!this.#reordered &&
			end !== undefined
		) {
			await this.#finishEncryptedGroup(end);
		}

		this.#closeGroup();
	}

	async #finishEncryptedGroup(end: Time.Micro): Promise<void> {
		const payload = await this.#encrypter!.encrypt(
			encodeFrame(new Uint8Array(), end),
		);

		this.#group!.writeFrame({
			payload,
			timestamp: Time.Timestamp.fromMicros(end),
		});
	}

	#validateEnd(end?: Time.Micro): void {
		if (
			this.#format.kind === "video" &&
			!this.#reordered &&
			end !== undefined &&
			this.#previous !== undefined &&
			end < this.#previous
		) {
			throw new Error("video group endpoint precedes its last frame");
		}
	}

	#estimatedEnd(): Time.Micro | undefined {
		if (this.#end === undefined || this.#interval === undefined) {
			return undefined;
		}

		return (this.#end + this.#interval) as Time.Micro;
	}

	#closeGroup(): void {
		this.#group!.close();
		this.#group = undefined;

		if (this.#end !== undefined) {
			this.#updateLiveEdge(this.#end);
		}

		this.#end = undefined;
		this.#previous = undefined;
		this.#reordered = false;
	}

	#updateLiveEdge(timestamp: Time.Micro): void {
		this.#liveEdge =
			this.#liveEdge === undefined
				? timestamp
				: (Math.max(this.#liveEdge, timestamp) as Time.Micro);
	}

	/** Close the track and current group, optionally with an error. */
	close(err?: Error): void | Promise<void> {
		if (err) {
			this.#group?.close(err);
			this.#track.close(err);
			return;
		}

		if (!this.#encrypter) {
			this.#close();
			this.#group?.close();
			this.#track.close();
			return;
		}

		return this.#closeEncryptedTrack();
	}

	async #closeEncryptedTrack(): Promise<void> {
		await this.#closeEncrypted();
		this.#group?.close();
		this.#track.close();
	}

	#refuse(timestamp: Time.Micro): void {
		const floor = this.#start;

		if (floor !== undefined && timestamp < floor) {
			throw new Error("frame timestamp is below the previous group start");
		}
	}
}
