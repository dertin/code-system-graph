const express = require("express");
const items = require("../controllers/items");

const router = express.Router();
router.get("/:id", items.read);

module.exports = router;
