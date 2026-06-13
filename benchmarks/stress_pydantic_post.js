import http from 'k6/http';

export let options = {
    vus: 20,
    duration: '30s',
};

const payload = JSON.stringify({
    user: {
        name: 'Preetham',
        age: 21
    },
    address: {
        street: '123 Main St',
        city: 'Hyderabad',
        zip: '500001'
    }
});

const params = {
    headers: {
        'Content-Type': 'application/json',
    },
};

export default function () {
    http.post('http://127.0.0.1:8000/register', payload, params);
}
