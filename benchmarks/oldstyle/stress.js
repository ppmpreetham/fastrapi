import http from "k6/http";

export const options = { vus: 20, duration: "30s" };
const BASE_URL = __ENV.BASE_URL || "http://127.0.0.1:8000";
const payload = JSON.stringify({ name: "Preetham", age: 21, active: true, score: 1.5 });
const params = { headers: { "Content-Type": "application/json" } };

export default function () { http.post(`${BASE_URL}/`, payload, params); }
