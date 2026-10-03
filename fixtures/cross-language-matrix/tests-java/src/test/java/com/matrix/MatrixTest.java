package com.matrix;

import org.junit.jupiter.api.Test;
import org.springframework.web.client.RestTemplate;

class MatrixTest {
    private static final String FASTAPI_URL = "http://fastapi-svc:8000";
    private static final String FLASK_URL = "http://flask-svc:5000";
    private static final String EXPRESS_URL = "http://express-svc:3000";
    private static final String NEST_URL = "http://nest-svc:3000";
    private static final String NEXT_URL = "http://next-svc:3000";
    private static final String SPRING_URL = "http://spring-svc:8080";
    private static final String GIN_URL = "http://gin-svc:8080";
    private static final String CHI_URL = "http://chi-svc:8080";
    private static final String NETHTTP_URL = "http://nethttp-svc:8080";
    private static final String AXUM_URL = "http://axum-svc:3000";
    private static final String ACTIX_URL = "http://actix-svc:8080";

    private final RestTemplate restTemplate = new RestTemplate();

    @Test
    void readsFastapiItem() {
        restTemplate.getForObject(FASTAPI_URL + "/fastapi/items/42", String.class);
    }

    @Test
    void readsFlaskItem() {
        restTemplate.getForObject(FLASK_URL + "/flask/items/42", String.class);
    }

    @Test
    void readsExpressItem() {
        restTemplate.getForObject(EXPRESS_URL + "/express/items/42", String.class);
    }

    @Test
    void readsNestItem() {
        restTemplate.getForObject(NEST_URL + "/nest/items/42", String.class);
    }

    @Test
    void readsNextItem() {
        restTemplate.getForObject(NEXT_URL + "/api/next/items/42", String.class);
    }

    @Test
    void readsSpringItem() {
        restTemplate.getForObject(SPRING_URL + "/spring/items/42", String.class);
    }

    @Test
    void readsGinItem() {
        restTemplate.getForObject(GIN_URL + "/gin/items/42", String.class);
    }

    @Test
    void readsChiItem() {
        restTemplate.getForObject(CHI_URL + "/chi/items/42", String.class);
    }

    @Test
    void readsNethttpItem() {
        restTemplate.getForObject(NETHTTP_URL + "/nethttp/items/42", String.class);
    }

    @Test
    void readsAxumItem() {
        restTemplate.getForObject(AXUM_URL + "/axum/items/42", String.class);
    }

    @Test
    void readsActixItem() {
        restTemplate.getForObject(ACTIX_URL + "/actix/items/42", String.class);
    }
}
