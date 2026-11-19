//! One small made-up repository per language: the lines the map must draw, and so also those it must not.

use code_map::{File, link, read};

/// The lines between the files given as path and text, each as "user -> used".
fn lines(files: &[(&str, &str)]) -> Vec<String> {
    let read: Vec<File> = files
        .iter()
        .map(|(path, text)| {
            let reading = read(path, text.as_bytes());
            File {
                path: path.to_string(),
                scope: reading.scope,
                names: reading
                    .symbols
                    .into_iter()
                    .map(|symbol| symbol.name)
                    .collect(),
                imports: reading.imports.into_iter().collect(),
                mentions: reading.mentions.into_iter().collect(),
            }
        })
        .collect();
    let line = |(user, used): (usize, usize)| format!("{} -> {}", files[user].0, files[used].0);
    link(&read).into_iter().map(line).collect()
}

#[test]
fn php() {
    let found = lines(&[
        (
            "app/Models/User.php",
            "<?php\nnamespace App\\Models;\nclass User { public function save() {} }",
        ),
        (
            "app/Models/Team.php",
            "<?php\nnamespace App\\Models;\nclass Team { function owner(): User { return new User(); } }",
        ),
        (
            "app/Http/Show.php",
            "<?php\nnamespace App\\Http;\nuse App\\Models\\User;\nuse Illuminate\\Support\\Str;\nclass Show { function run() { return User::find(1)->save(); } }",
        ),
        (
            "app/Http/Both.php",
            "<?php\nnamespace App\\Http;\nuse App\\Models\\{User, Team};\nclass Both extends Show {}",
        ),
        // The same class a second time, where its namespace does not lead, and a method that only shares a name.
        (
            "Models/User.php",
            "<?php\nnamespace App\\Models;\nclass User {}",
        ),
        (
            "lib/Str.php",
            "<?php\nnamespace Lib;\nclass Str { function find() {} }",
        ),
    ]);
    assert_eq!(
        found,
        [
            "app/Models/Team.php -> app/Models/User.php",
            "app/Http/Show.php -> app/Models/User.php",
            "app/Http/Both.php -> app/Models/User.php",
            "app/Http/Both.php -> app/Models/Team.php",
            "app/Http/Both.php -> app/Http/Show.php",
        ]
    );
}

#[test]
fn javascript_typescript_vue_and_svelte() {
    let found = lines(&[
        (
            "src/price.ts",
            "export function format(n: number) { return n.toFixed(2) }",
        ),
        (
            "src/lib/api/index.ts",
            "export const get = () => fetch('/')\nexport function post() {}",
        ),
        (
            "src/cart.ts",
            "import { format } from './price'\nimport { get } from '@/lib/api'\nimport vue from 'vue'\nexport const total = () => format(1)",
        ),
        (
            "src/old.js",
            "const price = require('./price.ts')\nexport * from './cart'",
        ),
        (
            "src/Card.vue",
            "<script setup lang=\"ts\">\nimport { total } from './cart'\n</script>\n<template><div /></template>",
        ),
        (
            "src/Page.vue",
            "<script setup>\nimport Card from './Card.vue'\n</script>\n<template><Card /><Missing /></template>",
        ),
        (
            "src/Row.svelte",
            "<script>\nimport { format } from './price'\n</script>\n<p>{format(1)}</p>",
        ),
        (
            "src/App.tsx",
            "import logo from './logo.svg'\nexport const App = () => <Shell />",
        ),
        ("src/Shell.tsx", "export function Shell() { return null }"),
        ("src/logo.svg", "<svg />"),
        // Defines names the others call, and is imported by none of them.
        (
            "vendor/vue.js",
            "export function post() {}\nexport function fetch() {}",
        ),
    ]);
    assert_eq!(
        found,
        [
            "src/cart.ts -> src/price.ts",
            "src/cart.ts -> src/lib/api/index.ts",
            "src/old.js -> src/price.ts",
            "src/old.js -> src/cart.ts",
            "src/Card.vue -> src/cart.ts",
            "src/Page.vue -> src/Card.vue",
            "src/Row.svelte -> src/price.ts",
            "src/App.tsx -> src/Shell.tsx",
            "src/App.tsx -> src/logo.svg",
        ]
    );
}

#[test]
fn python() {
    let found = lines(&[
        ("shop/__init__.py", ""),
        ("shop/price.py", "def format(n):\n    return str(n)\n"),
        (
            "shop/cart.py",
            "from .price import format\nimport os\n\ndef total():\n    return format(1)\n",
        ),
        (
            "shop/api/views.py",
            "from shop import cart\nfrom ..price import format\nimport shop.cart as c\n",
        ),
        ("tools/run.py", "import shop\nfrom shop.cart import total\n"),
        ("other/price.py", "def format(n):\n    pass\n"),
    ]);
    assert_eq!(
        found,
        [
            "shop/cart.py -> shop/price.py",
            "shop/api/views.py -> shop/__init__.py",
            "shop/api/views.py -> shop/price.py",
            "shop/api/views.py -> shop/cart.py",
            "tools/run.py -> shop/__init__.py",
            "tools/run.py -> shop/cart.py",
        ]
    );
}

#[test]
fn rust() {
    let found = lines(&[
        (
            "src/lib.rs",
            "mod db;\nmod map;\nmod pages;\npub fn now() -> i64 { 0 }",
        ),
        (
            "src/db.rs",
            "pub struct Doc;\npub fn docs() -> Vec<Doc> { vec![] }",
        ),
        (
            "src/map.rs",
            "use crate::db::{self, Doc};\nuse std::collections::HashMap;\npub fn round() { db::docs(); }",
        ),
        (
            "src/pages/mod.rs",
            "use super::map;\nuse serde::Serialize;\npub fn show() { map::round(); }",
        ),
        ("other/src/db.rs", "pub struct Doc;"),
    ]);
    assert_eq!(
        found,
        [
            "src/lib.rs -> src/db.rs",
            "src/lib.rs -> src/map.rs",
            "src/lib.rs -> src/pages/mod.rs",
            "src/map.rs -> src/db.rs",
            "src/pages/mod.rs -> src/map.rs",
        ]
    );
}

#[test]
fn go() {
    let found = lines(&[
        (
            "cmd/main.go",
            "package main\nimport (\n\t\"fmt\"\n\t\"shop/internal/store\"\n)\nfunc main() { store.Open(); fmt.Println() }",
        ),
        (
            "internal/store/open.go",
            "package store\nfunc Open() *Store { return load() }",
        ),
        (
            "internal/store/load.go",
            "package store\ntype Store struct{}\nfunc load() *Store { return nil }\nfunc Close() {}",
        ),
        (
            "internal/other/open.go",
            "package other\nfunc Open() {}\nfunc Println() {}",
        ),
    ]);
    assert_eq!(
        found,
        [
            "cmd/main.go -> internal/store/open.go",
            "internal/store/open.go -> internal/store/load.go",
        ]
    );
}

#[test]
fn java() {
    let found = lines(&[
        (
            "src/shop/model/User.java",
            "package shop.model;\npublic class User { public void save() {} }",
        ),
        (
            "src/shop/model/Team.java",
            "package shop.model;\npublic class Team { User owner; }",
        ),
        (
            "src/shop/web/Show.java",
            "package shop.web;\nimport shop.model.User;\nimport java.util.List;\nclass Show { void run() { new User().save(); } }",
        ),
        (
            "src/shop/web/All.java",
            "package shop.web;\nimport shop.model.*;\nclass All { Team team; }",
        ),
        (
            "src/other/User.java",
            "package other;\npublic class User {}",
        ),
    ]);
    assert_eq!(
        found,
        [
            "src/shop/model/Team.java -> src/shop/model/User.java",
            "src/shop/web/Show.java -> src/shop/model/User.java",
            "src/shop/web/All.java -> src/shop/model/Team.java",
        ]
    );
}

#[test]
fn c_and_cpp() {
    let found = lines(&[
        ("src/price.h", "int format(int n);"),
        (
            "src/price.c",
            "#include \"price.h\"\n#include <stdio.h>\nint format(int n) { return n; }",
        ),
        (
            "src/cart.cpp",
            "#include \"price.h\"\n#include \"util/log.hpp\"\nint total() { return format(1); }",
        ),
        ("src/util/log.hpp", "void log();"),
        ("old/price.h", "int format(int n);"),
    ]);
    assert_eq!(
        found,
        [
            "src/price.c -> src/price.h",
            "src/cart.cpp -> src/price.h",
            "src/cart.cpp -> src/util/log.hpp",
        ]
    );
}

#[test]
fn c_sharp() {
    let found = lines(&[
        (
            "Models/User.cs",
            "namespace Shop.Models { public class User { } }",
        ),
        (
            "Models/Team.cs",
            "namespace Shop.Models;\npublic class Team { public User Owner() { return new User(); } }",
        ),
        (
            "Web/Show.cs",
            "using System;\nusing Shop.Models;\nnamespace Shop.Web;\npublic class Show { public User Run() { return new User(); } }",
        ),
        (
            "Web/Idle.cs",
            "using Shop.Models;\nnamespace Shop.Web;\npublic class Idle { }",
        ),
        ("Old/User.cs", "namespace Old { public class User { } }"),
    ]);
    assert_eq!(
        found,
        [
            "Models/Team.cs -> Models/User.cs",
            "Web/Show.cs -> Models/User.cs",
        ]
    );
}

#[test]
fn ruby() {
    let found = lines(&[
        (
            "lib/shop/price.rb",
            "class Price\n  def self.format(n); n; end\nend",
        ),
        (
            "lib/shop/cart.rb",
            "require_relative \"price\"\nrequire \"json\"\nclass Cart\n  def total; format(1); end\nend",
        ),
        ("bin/run.rb", "require \"shop/cart\"\nCart.new.total"),
        // Rails finds a class by its name alone.
        ("app/models/user.rb", "class User < Record\nend"),
        (
            "app/models/record.rb",
            "class Record\n  def total; end\nend",
        ),
    ]);
    assert_eq!(
        found,
        [
            "lib/shop/cart.rb -> lib/shop/price.rb",
            "bin/run.rb -> lib/shop/cart.rb",
            "app/models/user.rb -> app/models/record.rb",
        ]
    );
}

#[test]
fn swift() {
    let found = lines(&[
        ("Sources/Shop/User.swift", "struct User { func save() {} }"),
        (
            "Sources/Shop/Price.swift",
            "enum Price { static func format(_ n: Int) -> Int { n } }",
        ),
        (
            "Sources/Shop/Cart.swift",
            "import Foundation\nstruct Cart { var user: User\n func total() -> Int { Price.format(1) } }",
        ),
        (
            "Sources/App/Main.swift",
            "import Shop\nlet cart = Cart(user: User())\ncart.save()",
        ),
    ]);
    assert_eq!(
        found,
        [
            "Sources/Shop/Cart.swift -> Sources/Shop/User.swift",
            "Sources/Shop/Cart.swift -> Sources/Shop/Price.swift",
            "Sources/App/Main.swift -> Sources/Shop/User.swift",
            "Sources/App/Main.swift -> Sources/Shop/Cart.swift",
        ]
    );
}

#[test]
fn dart() {
    let found = lines(&[
        ("lib/price.dart", "int format(int n) => n;"),
        (
            "lib/cart.dart",
            "import 'package:shop/price.dart';\nimport 'dart:io';\nclass Cart { int total() => format(1); }",
        ),
        ("lib/src/util.dart", "void log() {}"),
        (
            "lib/shop.dart",
            "export 'src/util.dart';\nimport 'cart.dart' as c;\nclass Page { final Cart cart = Cart(); }",
        ),
        ("test/price.dart", "int format(int n) => 0;"),
    ]);
    assert_eq!(
        found,
        [
            "lib/cart.dart -> lib/price.dart",
            "lib/shop.dart -> lib/cart.dart",
            "lib/shop.dart -> lib/src/util.dart",
        ]
    );
}

#[test]
fn elixir() {
    let found = lines(&[
        (
            "lib/shop/price.ex",
            "defmodule Shop.Price do\n  def format(n), do: n\nend",
        ),
        ("lib/shop/user.ex", "defmodule Shop.User do\nend"),
        ("lib/shop/team.ex", "defmodule Shop.Team do\nend"),
        (
            "lib/shop/cart.ex",
            "defmodule Shop.Cart do\n  alias Shop.Price\n  import Enum\n  def total, do: Price.format(1)\nend",
        ),
        (
            "lib/shop/web.ex",
            "defmodule Shop.Web do\n  alias Shop.{User, Team}\n  def cart, do: Shop.Cart.total()\nend",
        ),
    ]);
    assert_eq!(
        found,
        [
            "lib/shop/cart.ex -> lib/shop/price.ex",
            "lib/shop/web.ex -> lib/shop/user.ex",
            "lib/shop/web.ex -> lib/shop/team.ex",
            "lib/shop/web.ex -> lib/shop/cart.ex",
        ]
    );
}

#[test]
fn lua() {
    let found = lines(&[
        (
            "shop/price.lua",
            "local M = {}\nfunction M.format(n) return n end\nreturn M",
        ),
        ("shop/util/init.lua", "function log() end"),
        (
            "shop/cart.lua",
            "local price = require(\"shop.price\")\nlocal util = require \"shop.util\"\nlocal json = require(\"json\")\nfunction total() return price.format(1) end",
        ),
        ("other/price.lua", "function format(n) end"),
    ]);
    assert_eq!(
        found,
        [
            "shop/cart.lua -> shop/price.lua",
            "shop/cart.lua -> shop/util/init.lua",
        ]
    );
}

#[test]
fn bash() {
    let found = lines(&[
        ("scripts/lib/price.sh", "format() { echo \"$1\"; }"),
        ("scripts/lib/util.sh", "log() { echo; }"),
        (
            "scripts/run.sh",
            "source ./lib/price.sh\n. lib/util.sh\nsource /etc/profile\nformat 1",
        ),
        ("old/price.sh", "format() { :; }"),
    ]);
    assert_eq!(
        found,
        [
            "scripts/run.sh -> scripts/lib/price.sh",
            "scripts/run.sh -> scripts/lib/util.sh",
        ]
    );
}

#[test]
fn zig() {
    let found = lines(&[
        ("src/price.zig", "pub fn format(n: i32) i32 { return n; }"),
        (
            "src/main.zig",
            "const std = @import(\"std\");\nconst price = @import(\"price.zig\");\npub fn main() void { _ = price.format(1); }",
        ),
        ("old/price.zig", "pub fn format(n: i32) i32 { return 0; }"),
    ]);
    assert_eq!(found, ["src/main.zig -> src/price.zig"]);
}

#[test]
fn kotlin_and_scala() {
    let found = lines(&[
        (
            "src/shop/model/User.kt",
            "package shop.model\nclass User { fun save() {} }",
        ),
        (
            "src/shop/model/Team.scala",
            "package shop.model\nclass Team { def owner(): User = new User() }",
        ),
        (
            "src/shop/model/Plan.scala",
            "package shop.model\nclass Plan",
        ),
        (
            "src/shop/web/Show.kt",
            "package shop.web\nimport shop.model.User\nimport kotlin.math.max\nclass Show { fun run(): User = User() }",
        ),
        (
            "src/shop/web/All.scala",
            "package shop.web\nimport shop.model.{Team, Plan => P}\nimport scala.util._\nclass All",
        ),
        (
            "src/shop/web/Any.kt",
            "package shop.web\nimport shop.model.*\nclass Any { val user = User() }",
        ),
        ("src/other/User.kt", "package other\nclass User"),
    ]);
    assert_eq!(
        found,
        [
            "src/shop/model/Team.scala -> src/shop/model/User.kt",
            "src/shop/web/Show.kt -> src/shop/model/User.kt",
            "src/shop/web/All.scala -> src/shop/model/Team.scala",
            "src/shop/web/All.scala -> src/shop/model/Plan.scala",
            "src/shop/web/Any.kt -> src/shop/model/User.kt",
        ]
    );
}

#[test]
fn other_people_s_code_is_not_read() {
    let found = lines(&[
        (
            "src/Shop.php",
            "<?php\nuse Vendor\\Tool;\nclass Shop { function run() { return new Tool(); } }",
        ),
        (
            "vendor/acme/Tool.php",
            "<?php\nnamespace Vendor;\nclass Tool {}",
        ),
        (
            "web/node_modules/tool/index.js",
            "export function Tool() {}",
        ),
    ]);
    assert_eq!(found, [""; 0]);
    let read = read("vendor/acme/Tool.php", b"<?php\nclass Tool {}");
    assert_eq!((read.lines, read.symbols.len(), read.text), (2, 0, None));
}
