# Custom deck sheets and Tabletop Simulator

Findings from loading generated test sheets as custom decks in the game
(2026-11-22, unofficial observations, not a specification). Every slot of the
test sheets was filled with its own number, so the slot a card is taken from
is visible. Cards were spawned from JSON (`CustomDeck` with `NumWidth`,
`NumHeight`, `FaceURL`, `BackURL`, `BackIsHidden`, `UniqueBack: false`) from
local paths.

## Observed behaviour

| Test | Result |
|---|---|
| Sheet with 1 column and 5 rows (1x5) | Loads; all five cards show the right slot |
| Sheet with 5 columns and 1 row (5x1) | Loads; all five cards show the right slot |
| 10x7 sheet, card from the last slot (index 69), `BackIsHidden: true` | Shows the slot's image as a normal card face |
| 10x7 sheet, card from the last slot (index 69), `BackIsHidden: false` | Shows the slot's image as a normal card face |
| `BackIsHidden: true`, card in a hand, seen by another player | The separate back image |
| `BackIsHidden: false`, card in a hand, seen by another player | The **last slot of the face sheet**, for every card of the deck |

The in-game deck dialog does not accept fewer than 2 rows or columns, but a
deck described in JSON with 1 row or 1 column loads and works. Sheets made by
`ttskit` therefore use at least 2x2 unless `--columns 1` or `--rows 1` is given
explicitly.

### Consequences for the layout

- With `BackIsHidden: false` the last slot (bottom right) of the face sheet is
  the image other players see for a card in a hand. It cannot hold a normal
  card without every card in a hand showing that image to the others, so a
  10x7 sheet holds at most **69** cards.
- With `BackIsHidden: true` the hidden image is the separate back image and the
  last slot is free: a 10x7 sheet holds **70** cards.
- Both modes were tested with `UniqueBack: false` only. Decks with a unique
  back per card were not tested.

## Rules used by `ttskit image deck`

- Slots are numbered from 0, left to right and top to bottom. The slot of
  card `n` of a sheet is `n`; slot `columns * rows - 1` is the last one.
- A sheet has at most 10 columns and 7 rows.
- Unless `--no-hidden-slot` is given, the last slot is reserved for the
  hidden-card image (`BackIsHidden: false` in the game).
- With `--no-hidden-slot` all slots hold cards (up to 70 per sheet) and the
  deck must be set up with `BackIsHidden: true` in the game.
- `ttskit image deck split` reads the same rules backwards. From a save
  (`--save FILE --deck-id ID`) the grid is `NumWidth` x `NumHeight` of the
  `CustomDeck` entry with that key, and `BackIsHidden: true` means every slot
  is a card (no hidden-card slot), while `BackIsHidden: false` (or no key, the
  game's default) means the last slot is the hidden-card slot. The entry is
  repeated by every card and by the deck object, so all occurrences must agree.
  With only `--columns` and `--rows` the last slot is assumed to be the
  hidden-card slot, as in `build`.

## Manual check of `image deck build`

2026-11-22: 70 numbered cards were built twice and loaded in the game as custom
decks (cards spawned from JSON with `UniqueBack: false`).

- `--no-hidden-slot` (one 10x7 sheet, `BackIsHidden: true`): the cards of slots
  0, 1, 68 and 69 showed their own images.
- Default build (a 10x7 sheet with 69 cards plus a 2x2 sheet with 1 card,
  `BackIsHidden: false`): slots 0, 1 and 68 and the single card of the second
  sheet showed their own images; the hidden-card slot of both sheets showed the
  plain fill colour. A card of the first sheet in a hand showed its face to its
  owner and the plain hidden-card image to another player.

## Deck objects (`image deck object`)

The field set of `image deck object` was compared with a 60 card deck made in
the game's custom deck dialog (one sheet, `UniqueBack`, `Sideways` and
`BackIsHidden` all off). Observed there:

- The object is a `DeckCustom` with `DeckIDs` 100 to 159 and a `CustomDeck`
  with the single key `1`; each card is a `Card` with `CardID` 100 to 159 and no
  `CustomDeck` of its own. Card id = key times 100 plus the slot.
- A card has `Hands: true`, a deck has `Hands: false` and
  `HideWhenFaceDown: true`. The game gave every nickname, description and GM
  note as an empty string.

The test fixture `tests/fixtures/game_deck.json` is that deck cut down to three
cards with placeholder URLs; a test checks that the generated object has the
same keys in the same order.

Not observed in the game: the key order of a single `CardCustom` and a deck
with more than one sheet are built by the same rules, not copied from a game
made object. `UniqueBack: true` (one back sheet per face sheet) is likewise not
verified against the game's own output.

## Manual check of `image deck object`

2026-11-22, against a running game via the External Editor API
(`ttskit editor exec`, `spawnObjectData` with the generated object):

- A deck of 5 cards (sheet URLs of the deck above, `--nicknames-from-files`)
  spawned as a deck of type `Deck` with 5 cards; the nicknames were the file
  names and every card had its own GUID.
- A one-card object spawned as a `Card` with the nickname of its file.
- The first check used a deck with a 10x7 grid over a sheet image that is really 9x7, so
  the cards showed two card parts and the check proved nothing about the images. It is being
  repeated with the 9x7 grid, with `BackIsHidden` on and off (result pending).
