import * as Moq from "@moq/net";
import { Time } from "@moq/net";

export type { BufferedRange, BufferedRanges, Frame } from "./types";

import type { AudioConfig, VideoConfig } from "../catalog";
import type { Recorder as TimelineRecorder } from "../timeline";
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
		/*
		 * Legacy-format frames do not use a CMAF sequence number.
		 * The secure-frame interface still requires one, so use zero.
		 */
		const plaintext = await this.#decrypter!.decrypt(0, frame);

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

/** Options for a legacy-container {@link Producer}. */
export interface ProducerProps {
	/**
	 * Report each group open (sequence + start timestamp) into the broadcast's
	 * timeline, so consumers can index the media without downloading it.
	 */
	timeline?: TimelineRecorder;
}

/** Writes legacy-container frames into a MoQ track, starting a new group on each keyframe. */
export class Producer {
	#track: Moq.Track.Producer;
	#format: Format;
	#previous?: Time.Micro;
	#reordered = false;
	#group?: Moq.Group.Producer;
	#timeline?: TimelineRecorder;
	#encrypter?: FrameEncrypter;

	/** The newest timestamp written. */
	#end?: Time.Micro;

	/** Exclusive presentation end of finished groups. */
	#liveEdge?: Time.Micro;

	/** Gap between consecutive timestamps. */
	#interval?: Time.Micro;

	/** Whether a marker has already been written for the latest cut. */
	#marked = false;

	constructor(
		track: Moq.Track.Producer,
		format: Format,
		props: ProducerProps = {},
	) {
		this.#format = format;
		this.#track = track;
		this.#timeline = props.timeline;
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
	 *
	 * With no encrypter, the returned operation is synchronous.
	 * With an encrypter, the returned promise resolves after encryption.
	 */
	encode(
		data: Uint8Array | Source,
		timestamp: Time.Micro,
		keyframe: boolean,
	): void | Promise<void> {
		this.#marked = false;

		if (!this.#encrypter) {
			return this.#encodePlaintext(data, timestamp, keyframe);
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

			if (rewound) {
				this.#interval = undefined;
			}

			this.#refuse(timestamp);
			this.#group = this.#track.appendGroup();

			this.#timeline?.record(
				this.#group.sequence,
				timestamp,
				true,
			);
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		} else {
			this.#refuse(timestamp);
		}

		this.#group.writeFrame({
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

			await this.#cutEncrypted(rewound ? undefined : timestamp);

			if (rewound) {
				this.#interval = undefined;
			}

			this.#refuse(timestamp);
			this.#group = this.#track.appendGroup();

			this.#timeline?.record(
				this.#group.sequence,
				timestamp,
				true,
			);
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		} else {
			this.#refuse(timestamp);
		}

		const payload = await this.#encodeEncryptedFrame(
			data,
			timestamp,
		);

		this.#group.writeFrame({
			payload,
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});

		this.#recordFrame(timestamp);
	}

	/**
	 * Encode the complete legacy payload first, then encrypt it.
	 */
	async #encodeEncryptedFrame(
		data: Uint8Array | Source,
		timestamp: Time.Micro,
	): Promise<Uint8Array> {
		const plaintext = encodeFrame(data, timestamp);

		return this.#encrypter!.encrypt(
			this.#group!.sequence,
			plaintext,
		);
	}

	#recordFrame(timestamp: Time.Micro): void {
		this.#reordered ||=
			this.#previous !== undefined &&
			timestamp < this.#previous;

		if (
			this.#previous !== undefined &&
			timestamp > this.#previous
		) {
			this.#interval = (
				timestamp - this.#previous
			) as Time.Micro;
		}

		this.#previous = timestamp;

		if (this.#end === undefined || timestamp > this.#end) {
			this.#end = timestamp;
		}
	}

	/**
	 * Flush and close the current group at the supplied or estimated end
	 * timestamp, then mark a break in the timeline.
	 */
	cut(end?: Time.Micro): void | Promise<void> {
		if (!this.#encrypter) {
			return this.#cutPlaintext(end);
		}

		return this.#cutEncryptedWithMarker(end);
	}

	#cutPlaintext(end?: Time.Micro): void {
		this.#close(end);

		// Nothing is measured across the break.
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

		this.#timeline?.record(
			group.sequence,
			timestamp,
			false,
		);
		this.#timeline?.end(timestamp);

		group.writeFrame({
			payload: encodeFrame(new Uint8Array(), timestamp),
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});

		group.close();

		this.#liveEdge =
			this.#liveEdge === undefined
				? timestamp
				: (Math.max(
					this.#liveEdge,
					timestamp,
				) as Time.Micro);

		this.#marked = true;
	}

	async #cutEncryptedWithMarker(
		end?: Time.Micro,
	): Promise<void> {
		await this.#cutEncrypted(end);

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

		this.#timeline?.record(
			group.sequence,
			timestamp,
			false,
		);
		this.#timeline?.end(timestamp);

		const payload = await this.#encrypter!.encrypt(
			group.sequence,
			encodeFrame(new Uint8Array(), timestamp),
		);

		group.writeFrame({
			payload,
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});

		group.close();

		this.#liveEdge =
			this.#liveEdge === undefined
				? timestamp
				: (Math.max(
					this.#liveEdge,
					timestamp,
				) as Time.Micro);

		this.#marked = true;
	}

	/**
	 * Close the current group without writing a marker group.
	 */
	#close(end?: Time.Micro): void {
		if (!this.#group) {
			return;
		}

		this.#validateEnd(end);

		end ??= this.#estimatedEnd();

		this.#format.finishGroup(
			this.#group,
			this.#reordered ? undefined : end,
		);

		this.#closeGroup(end);
	}

	async #cutEncrypted(end?: Time.Micro): Promise<void> {
		if (!this.#group) {
			return;
		}

		this.#validateEnd(end);

		end ??= this.#estimatedEnd();

		if (
			this.#format.kind === "video" &&
			!this.#reordered &&
			end !== undefined
		) {
			await this.#finishEncryptedGroup(end);
		}

		this.#closeGroup(end);
	}

	async #finishEncryptedGroup(end: Time.Micro): Promise<void> {
		const plaintext = encodeFrame(
			new Uint8Array(),
			end,
		);

		const payload = await this.#encrypter!.encrypt(
			this.#group!.sequence,
			plaintext,
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
			throw new Error(
				"video group endpoint precedes its last frame",
			);
		}
	}

	#estimatedEnd(): Time.Micro | undefined {
		if (
			this.#end === undefined ||
			this.#interval === undefined
		) {
			return undefined;
		}

		return (
			this.#end + this.#interval
		) as Time.Micro;
	}

	#closeGroup(end?: Time.Micro): void {
		const bound = end ?? this.#end;

		if (bound !== undefined) {
			this.#timeline?.end(bound);
		}

		this.#group!.close();
		this.#group = undefined;

		if (this.#end !== undefined) {
			this.#liveEdge =
				this.#liveEdge === undefined
					? this.#end
					: (Math.max(
						this.#liveEdge,
						this.#end,
					) as Time.Micro);
		}

		this.#end = undefined;
		this.#previous = undefined;
		this.#reordered = false;
	}

	#refuse(timestamp: Time.Micro): void {
		if (
			this.#liveEdge !== undefined &&
			timestamp < this.#liveEdge
		) {
			throw new Error(
				"frame timestamp is below the live edge",
			);
		}
	}

	/** Close the track and current group, optionally with an error. */
	close(err?: Error): void | Promise<void> {
		if (err) {
			this.#group?.close(err);
			this.#track.close(err);
			return;
		}

		if (!this.#encrypter) {
			this.#cutPlaintext();
			this.#group?.close();
			this.#track.close();
			return;
		}

		return this.#closeEncrypted();
	}

	async #closeEncrypted(err?: Error): Promise<void> {
		if (err) {
			this.#group?.close(err);
			this.#track.close(err);
			return;
		}

		await this.#cutEncrypted();

		this.#group?.close();
		this.#track.close();
	}
}
