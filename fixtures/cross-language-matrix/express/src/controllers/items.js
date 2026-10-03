exports.read = (req, res) => {
  res.json({ id: req.params.id });
};
