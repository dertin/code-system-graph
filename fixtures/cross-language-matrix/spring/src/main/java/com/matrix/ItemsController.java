package com.matrix;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PathVariable;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
@RequestMapping("/spring/items")
public class ItemsController {
    @GetMapping("/{id}")
    public Item read(@PathVariable String id) {
        return new Item(id);
    }
}
