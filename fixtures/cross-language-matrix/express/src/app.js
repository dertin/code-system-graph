const express = require("express");
const items = require("./routes/items");

const app = express();
app.use("/express/items", items);

module.exports = app;
