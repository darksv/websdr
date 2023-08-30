class SocketClient {
    #isOpen = false;
    #socket = null
    #frequency = 0;

    constructor({onFft, onSamples}) {
        const url = (location.protocol === 'https:' ? 'wss' : 'ws') + '://' + location.hostname + ':' + location.port + '/ws';
        this.#socket = new WebSocket(url);
        this.#socket.binaryType = 'arraybuffer';
        this.#socket.addEventListener('open', (e) => {
            this.#isOpen = true;
            this.#sendFrequency(this.#frequency);
        });
        this.#socket.addEventListener('message', (e) => {
            if (!(e.data instanceof ArrayBuffer)) {
                const msg = JSON.parse(e.data);
                updateInfo({frequency: msg});
                return;
            }

            const data = new Uint8Array(e.data);
            if (data.length < 1) {
                return;
            }

            if (data[0] === FRAME_FFT) {
                onFft(data.slice(1));
            } else if (data[0] === FRAME_AUDIO) {
                const samples = new DataView(e.data);
                const numSamples = (samples.buffer.byteLength - 1) / 2;

                const buffer = new Float32Array(numSamples);
                for (let i = 0; i < numSamples; i++) {
                    buffer[i] = (samples.getInt16(1 + i * 2, true) / 32767.0);
                }
                onSamples(buffer);
            }
        });
    }

    set frequency(frequency) {
        this.#frequency = frequency;
        if (this.#isOpen) {
            this.#sendFrequency(frequency);
        }
    }

    #sendFrequency(frequency) {
        this.#socket.send(JSON.stringify({
            command: 'change_frequency',
            frequency: frequency
        }));
    }
}

const maxChunkSize = 50;
let lastBlockHeight = 0;
let canvasContainer = document.getElementById('spectrogram');

const FRAME_FFT = 0x01;
const FRAME_AUDIO = 0x02;

const pushFftFrame = (fft) => {
    if (canvasContainer.childNodes.length === 0 || lastBlockHeight >= maxChunkSize) {
        const canvas = document.createElement('canvas');
        canvas.width = 4096;
        canvas.height = maxChunkSize;
        canvasContainer.prepend(canvas);
        lastBlockHeight = 0;
    }

    const canvas = canvasContainer.childNodes[0];
    const ctx = canvas.getContext('2d');
    const imageData = ctx.createImageData(4096, 1);
    // Iterate through every pixel
    for (let i = 0; i < 4096; i++) {
        imageData.data[4 * i + 0] = fft[i];  // R value
        imageData.data[4 * i + 1] = 0;       // G value
        imageData.data[4 * i + 2] = 0;       // B value
        imageData.data[4 * i + 3] = 255;     // A value
    }

    ctx.putImageData(imageData, 0, maxChunkSize - lastBlockHeight);
    lastBlockHeight++;

    for (let i = canvasContainer.children.length - 1; i >= 0; i--) {
        const child = canvasContainer.children[i];
        if (child.getBoundingClientRect().top >= document.documentElement.clientHeight) {
            canvasContainer.removeChild(child);
            continue;
        }
        child.style.transform = 'translateY(' + (i * (maxChunkSize - 1) - (maxChunkSize - lastBlockHeight + 1)) + 'px)';
    }
};

const playAudioFrame = (samples) => {
    if (audioWorkerPort) {
        audioWorkerPort.postMessage({samples});
    }
};

const client = new SocketClient({
    onFft: pushFftFrame,
    onSamples: playAudioFrame,
});

let audioWorkerPort = null;
let audioInitialized = false;
let audioGainNode = null;
let config = {
    volume: 0.2,
    frequency: 100100000,
}

const initAudio = async () => {
    if (audioInitialized) {
        return;
    }
    console.info('initializing audio');

    audioInitialized = true;

    const audioCtx = new window.AudioContext({
        sampleRate: 22050
    });

    await audioCtx.audioWorklet.addModule('worklet.js');
    console.info('worklet loaded');

    const gainNode = audioCtx.createGain();
    const sourceNode = new AudioWorkletNode(
        audioCtx,
        'buffered-audio-chunk-processor'
    );
    sourceNode.port.onmessage = (e) => {
        updateInfo(e.data);
    };

    audioWorkerPort = sourceNode.port;
    audioGainNode = gainNode;
    sourceNode.connect(gainNode);
    gainNode.connect(audioCtx.destination);
};

const updateInfo = ({bufferSize, bufferCapacity, frequency, volume}) => {
    if (bufferSize !== undefined && bufferCapacity !== undefined) {
        const fullness = bufferSize / bufferCapacity * 100;
        document.getElementById('buffer-info').innerText = `Buffer: ${bufferSize}/${bufferCapacity} (${~~fullness}%)`;
    }

    if (frequency !== undefined) {
        const freq = frequency / 1_000_000;
        document.getElementById('frequency-info').innerText = `Frequency: ${freq.toFixed(3)}MHz`;
    }

    if (volume !== undefined) {
        document.getElementById('volume-control').value = volume * 100;
    }
}

const updateFrequency = (callback) => {
    if (callback) {
        config.frequency = callback(config.frequency);
    }

    client.frequency = config.frequency;
    updateInfo({
        frequency: config.frequency,
        bufferCapacity: 0,
        bufferSize: 0,
        volume: config.volume
    });
};

const updateLocation = () => {
    const url = new URL(location);
    const freq = url.searchParams.get('frequency');
    if (freq) {
        updateFrequency(_ => parseInt(freq))
    }
};

window.addEventListener('load', (e) => {
    updateLocation();
});

window.addEventListener('locationchange', (e) => {
    updateLocation();
});

window.addEventListener('popstate', (e) => {
    updateLocation();
});

document.getElementById('frequency-info').addEventListener('wheel', (e) => {
    updateFrequency(curr => curr + Math.sign(e.deltaY) * 100_000);

    const url = new URL(location);
    url.searchParams.set('frequency', config.frequency);
    history.pushState({}, '', url);
});

document.getElementById('volume-control').addEventListener('change', e => {
    config.volume = e.target.value / 100;
    initAudio().then(() => {
        audioGainNode.gain.setValueAtTime(config.volume, 0);
    });
});
