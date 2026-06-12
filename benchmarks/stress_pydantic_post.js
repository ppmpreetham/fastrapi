import http from 'k6/http';

export let options = {
    vus: 20,
    duration: '30s',
};

const payload = JSON.stringify({
    name: 'Preetham',
    age: 21,
    active: true,
    score: 99.5,
});

const params = {
    headers: {
        'Content-Type': 'application/json',
    },
};

export default function () {
    http.post('http://127.0.0.1:8000/', payload, params);
}
