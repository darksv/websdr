const url = (location.protocol === 'https:' ? 'wss' : 'ws') + '://' + location.hostname + ':' + location.port + '/ws';
const socket = new WebSocket(url);
socket.binaryType = 'arraybuffer';
socket.addEventListener('open', function (event) {
    // socket.send('Hello Server!');
});

const maxChunkSize = 50;
let lastBlockHeight = 0;
let canvasContainer = document.getElementById('spectrogram');

const FRAME_FFT = 0x01;
const FRAME_AUDIO = 0x02;

socket.addEventListener('message', function (event) {
    if (!(event.data instanceof ArrayBuffer)) {
        const msg = JSON.parse(event.data);
        updateInfo({frequency: msg});
        return;
    }

    const data = new Uint8Array(event.data);
    if (data.length < 1) {
        return;
    }

    if (data[0] === FRAME_FFT) {
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
            imageData.data[4 * i + 0] = data[1 + i];  // R value
            imageData.data[4 * i + 1] = 0;            // G value
            imageData.data[4 * i + 2] = 0;            // B value
            imageData.data[4 * i + 3] = 255;          // A value
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
    } else if (data[0] === FRAME_AUDIO) {
        if (!audioWorkerPort) {
            return;
        }

        const samples = new DataView(event.data);
        const numSamples = (samples.buffer.byteLength - 1) / 2;

        const buffer = new Float32Array(numSamples);
        for (let i = 0; i < numSamples; i++) {
            buffer[i] = (samples.getInt16(1 + i * 2, true) / 32767.0);
        }
        audioWorkerPort.postMessage({samples: buffer});
    }
});

let audioWorkerPort = null;
let audioInitialized = false;
let audioGainNode = null;
let config = {
    volume: 0.2,
    frequency: 100100000,
}

const initAudio = () => {
    if (audioInitialized) {
        return;
    }

    audioInitialized = true;

    const audioCtx = new window.AudioContext({
        sampleRate: 22050
    });

    audioCtx.audioWorklet.addModule("worklet.js?x").then(() => {
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
    });
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

document.getElementById('frequency-info').addEventListener('wheel', (e) => {
    config.frequency += Math.sign(e.deltaY) * 100000;
    socket.send(JSON.stringify({command: "change_frequency", frequency: config.frequency}));
});

document.getElementById('volume-control').addEventListener('change', e => {
    initAudio();

    config.volume = e.target.value / 100;
    audioGainNode.gain.setValueAtTime(config.volume, 0);
});

updateInfo({frequency: config.frequency, bufferCapacity: 0, bufferSize: 0, volume: config.volume});