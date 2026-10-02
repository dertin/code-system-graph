import axios from "axios";

const FASTAPI_URL = "http://fastapi-svc:8000";
const FLASK_URL = "http://flask-svc:5000";
const EXPRESS_URL = "http://express-svc:3000";
const NEST_URL = "http://nest-svc:3000";
const NEXT_URL = "http://next-svc:3000";
const SPRING_URL = "http://spring-svc:8080";
const GIN_URL = "http://gin-svc:8080";
const CHI_URL = "http://chi-svc:8080";
const NETHTTP_URL = "http://nethttp-svc:8080";
const AXUM_URL = "http://axum-svc:3000";
const ACTIX_URL = "http://actix-svc:8080";

describe("matrix", () => {
  it("reads fastapi", async () => {
    await fetch(`${FASTAPI_URL}/fastapi/items/42`);
  });

  it("reads flask", async () => {
    await axios.get(`${FLASK_URL}/flask/items/42`);
  });

  it("reads express", async () => {
    await fetch(`${EXPRESS_URL}/express/items/42`);
  });

  it("reads nest", async () => {
    await axios.get(`${NEST_URL}/nest/items/42`);
  });

  it("reads next", async () => {
    await fetch(`${NEXT_URL}/api/next/items/42`);
  });

  it("reads spring", async () => {
    await axios.get(`${SPRING_URL}/spring/items/42`);
  });

  it("reads gin", async () => {
    await fetch(`${GIN_URL}/gin/items/42`);
  });

  it("reads chi", async () => {
    await axios.get(`${CHI_URL}/chi/items/42`);
  });

  it("reads nethttp", async () => {
    await fetch(`${NETHTTP_URL}/nethttp/items/42`);
  });

  it("reads axum", async () => {
    await axios.get(`${AXUM_URL}/axum/items/42`);
  });

  it("reads actix", async () => {
    await fetch(`${ACTIX_URL}/actix/items/42`);
  });
});
