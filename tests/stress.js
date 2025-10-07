// if you're testing this, run it with the command:
// k6 run tests/stress.js
// make sure that you have k6 installed: https://k6.io/docs/getting-started/installation/

import http from 'k6/http';

export let options = {
    vus: 20,
    duration: '30s',
};

export default function () {
    http.get('http://127.0.0.1:8080/hello');
}