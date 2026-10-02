import http from "k6/http";

export const options = { vus: 20, duration: "30s" };
const BASE_URL = __ENV.BASE_URL || "http://127.0.0.1:8000";

export default function () { http.get(`${BASE_URL}/`); }
