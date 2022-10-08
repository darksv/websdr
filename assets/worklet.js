class BufferedAudioChunkProcessor extends AudioWorkletProcessor {
    #buffer = new Float32Array(32 * 1024);
    #head = 0;
    #length = 0;
    #waitingToFill = false;
    #minLength = 4096;

    constructor(props) {
        super(props);

        this.port.onmessage = (e) => {
            const samples = e.data.samples;
            for (let i = 0; i < samples.length; i++) {
                this.#buffer[(this.#head + this.#length + i) % this.#buffer.length] = samples[i];
            }
            this.#length = Math.min(this.#buffer.length, this.#length + samples.length);
            this.#sendStats();
        };
    }

    process(inputs, outputs, _parameters) {
        const channel = outputs[0][0];

        if (this.#waitingToFill && this.#length < this.#minLength) {
            return true;
        }

        if (this.#length < channel.length) {
            this.#waitingToFill = true;
            return true;
        }

        if (this.#waitingToFill && this.#length >= this.#minLength) {
            this.#waitingToFill = false;
        }

        for (let i = 0; i < channel.length; i++) {
            channel[i] = this.#buffer[(this.#head + i) % this.#buffer.length];
        }
        this.#length -= channel.length;
        this.#head = (this.#head + channel.length) % this.#buffer.length;
        this.#sendStats();
        return true;
    }

    #sendStats() {
        this.port.postMessage({
            bufferSize: this.#length,
            bufferCapacity: this.#buffer.length
        });
    }
}

registerProcessor('buffered-audio-chunk-processor', BufferedAudioChunkProcessor);