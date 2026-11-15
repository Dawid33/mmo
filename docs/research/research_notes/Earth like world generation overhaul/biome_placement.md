# Realistic biome placement from simulated climate, terrain, soil and water (72 biomes)

**Reading guide — how far to trust each number.** Three trust levels are used throughout:

- **[V]** verified this session: the number was read from the fetched source (or from its abstract/snippet as returned by search; those are marked "[V-snippet]").
- **[R]** recalled from the literature, NOT re-verified this session (the source was paywalled or blocked: Prentice 1992, Holdridge 1967, Kleypas 1999, Ellis & Ramankutty 2008, Beck 2018 and others). The values are the standard ones, but check them against the paper before they become named constants.
- **[I]** my inference / a design recommendation, not a literature fact.

Sources that could not be opened at all: Beck et al. 2018/2023 (Nature redirect to login), Prentice et al. 1992 full table, Kaplan BIOME4, Holdridge 1967, Kleypas 1999 full text, Keith et al. 2022 (IUCN GET) profiles, Olson 2001 / Dinerstein 2017 area tables, Azgaar's `biomes.js` (404 on every path tried; values below are from a search snippet of the code), Dwarf Fortress wiki, Undiscovered Worlds, WorldEngine, Vintage Story, Songs of Syx, No Man's Sky. Those generator comparisons are therefore thin.

Derived per-cell quantities the rules below need (all computable from the planned fields) [I]:

| Symbol | Definition | From |
|---|---|---|
| Tcold, Thot | mean temperature of the coldest / warmest month (°C) | 12 monthly T |
| MAT, MAP | annual mean T (°C), annual precipitation (mm) | monthly T, P |
| Tmon10 | number of months with T > 10 °C | monthly T |
| GDD5, GDD0 | growing degree days above 5 / 0 °C ≈ Σ max(0, Tm − base) × days in month | monthly T |
| GS_len, GS_T | growing-season length (days with T > 0.9 °C, interpolated between months) and the mean T over it | monthly T |
| PET | potential evapotranspiration, mm/yr and per month (Thornthwaite from monthly T + latitude day length is the cheapest; Hargreaves if a diurnal range is available) | monthly T, latitude |
| AI | aridity index = MAP / PET | above |
| α | AET/PET from a one-bucket monthly soil-water balance (bucket size from soil depth/texture, default ~150 mm) | monthly P, PET, soil |
| DryM60, DryM100 | months with P < 60 mm, < 100 mm | monthly P |
| Pdry, Psdry, Pwdry, Pswet, Pwwet | Köppen seasonal precipitation statistics | monthly P, hemisphere |
| WT | water-table depth / soil wetness (existing "drainage" successor) | soil-water field |
| SSTmin, SSTmax | coldest / warmest monthly sea-surface temperature | monthly SST |

---

## 1. Which climate classification should be the backbone for zonal biomes?

### Takeaway
Use **Köppen–Geiger (Peel 2007 criteria) as the cheap, exactly specified first-pass climate label**, and split each Köppen class into the listed vegetation biomes with **BIOME1/LPJ-style plant limits** (coldest-month temperature, GDD5, a moisture index, dry-season length). Köppen alone does not separate forest from grassland within a class and Holdridge ignores seasonality entirely; the plant-limit layer is what gives conifer/deciduous/evergreen/savanna splits that look right.

### Cited Findings

**Köppen–Geiger criteria (Peel et al. 2007, Table 1) [V]** — temperatures in °C, precipitation in mm — [Peel et al. 2007, HESS 11:1633](https://hess.copernicus.org/articles/11/1633/2007/hess-11-1633-2007.pdf)

| Class | Criterion |
|---|---|
| A tropical | Tcold ≥ 18 |
| Af rainforest | Pdry ≥ 60 |
| Am monsoon | not Af and Pdry ≥ 100 − MAP/25 |
| Aw savannah | not Af and Pdry < 100 − MAP/25 |
| B arid | MAP < 10 × Pthreshold |
| BW desert | MAP < 5 × Pthreshold |
| BS steppe | MAP ≥ 5 × Pthreshold |
| h / k | MAT ≥ 18 / MAT < 18 |
| C temperate | Thot > 10 and 0 < Tcold < 18 |
| D cold | Thot > 10 and Tcold ≤ 0 |
| s dry summer | Psdry < 40 and Psdry < Pwwet/3 |
| w dry winter | Pwdry < Pswet/10 |
| f no dry season | neither s nor w |
| a hot summer | Thot ≥ 22 |
| b warm summer | not a and Tmon10 ≥ 4 |
| c cold summer | not a/b, 1 ≤ Tmon10 < 4 |
| d very cold winter (D only) | not a/b and Tcold < −38 |
| E polar | Thot < 10 |
| ET tundra / EF frost | Thot > 0 / Thot ≤ 0 |

- Pthreshold = 2×MAT if ≥70 % of MAP falls in winter; 2×MAT + 28 if ≥70 % falls in summer; otherwise 2×MAT + 14. Summer/winter = the warmer/cooler six months (ONDJFM vs AMJJAS). [V] — [Peel et al. 2007](https://hess.copernicus.org/articles/11/1633/2007/hess-11-1633-2007.pdf)
- B must be tested **before** A, C, D (every B cell also satisfies one of them); E is tested first of all in practice. Peel uses 0 °C, not Köppen's original −3 °C, for the C/D boundary. [V] — [Peel et al. 2007](https://hess.copernicus.org/articles/11/1633/2007/hess-11-1633-2007.pdf)
- When a cell satisfies both s and w (Cs and Cw), Peel assigns w if summer-half precipitation exceeds winter-half precipitation. Peel also notes the s/w/f criteria "still produce inappropriate results" at high latitudes and suggests seasonal-total (70 %) rules would be more robust. [V] — [Peel et al. 2007](https://hess.copernicus.org/articles/11/1633/2007/hess-11-1633-2007.pdf)
- Wikipedia's statement of the same scheme agrees (A ≥ 18 °C every month; Am driest month ≥ 100 − MAP/25; B threshold 20×MAT + 280/140/0 in mm; a > 22 °C; b ≥ 4 months > 10 °C; d coldest < −38 °C) and adds the optional modifier **n = frequent fog** for B climates (fog deserts). [V] — [Wikipedia: Köppen climate classification](https://en.wikipedia.org/wiki/K%C3%B6ppen_climate_classification)

**LPJ / LPJmL plant functional type bioclimatic limits [V]** ("similar as in Sitch et al. 2003") — [LPJmL4 supplement, GMD 2017, Table 2](https://gmd.copernicus.org/preprints/gmd-2017-145/gmd-2017-145-supplement.pdf)

| PFT | Tcold min (°C) | Tcold max (°C) | GDD5 min (°C·day) |
|---|---|---|---|
| Tropical broadleaved evergreen | 15.5 | – | – |
| Tropical broadleaved raingreen | 15.5 | – | – |
| Temperate needleleaved evergreen | −2.0 | 22 | 900 |
| Temperate broadleaved evergreen | 3.0 | 18.8 | 1200 |
| Temperate broadleaved summergreen | −17.7 | 15.5 | 1200 |
| Boreal needleleaved evergreen | −32.5 | −2.0 | 600 |
| Boreal broadleaved summergreen | – | −2.0 | 350 |
| Boreal needleleaved summergreen (larch) | −46.5 | −5.4 | 350 |
| Tropical herbaceous (C4) | 7.0 | – | (6500 listed in the GDD column) |
| Temperate herbaceous (C3) | −39.0 | 15.5 | – |
| Polar herbaceous | – | −2.6 | – |

**BIOME1** — the model is an environmental sieve (each PFT has limits on coldest-month temperature, GDD5/GDD0 and a moisture index) followed by a dominance hierarchy that picks the dominant PFTs among survivors; primary drivers are coldest-month mean temperature, GDD0/GDD5 and a moisture index. [V-snippet] — [Prentice et al. 1992, J. Biogeogr. 19:117 (IIASA copy)](https://pure.iiasa.ac.at/id/file/9128); method summary also in [Dallmeyer et al. 2019, Clim. Past 15:335](https://cp.copernicus.org/articles/15/335/2019/)

**Holdridge** — three axes: mean annual biotemperature, annual precipitation, PET ratio; both scales are logarithmic; biotemperature sets months below freezing **and above 30 °C** to 0 before averaging; 38 life zones in the IIASA version (polar desert … tropical rain forest, including "subtropical thorn woodland", "tropical very dry forest", "cool temperate steppe", "warm temperate desert scrub"). [V] — [Wikipedia: Holdridge life zones](https://en.wikipedia.org/wiki/Holdridge_life_zones)

### Inferences

**BIOME1 PFT limits [R]** (Prentice et al. 1992, Table 1; α = AET/PET Priestley–Taylor moisture index; not re-verified):

| PFT | Tcold min | Tcold max | GDD min | Twarm min | α min | α max |
|---|---|---|---|---|---|---|
| Tropical evergreen | 15.5 | | | | 0.80 | |
| Tropical raingreen | 15.5 | | | | 0.45 | 0.95 |
| Warm-temperate evergreen | 5 | | | | 0.65 | |
| Temperate summergreen | −15 | 15.5 | GDD5 1200 | | 0.65 | |
| Cool-temperate conifer | −19 | 5 | GDD5 900 | | 0.65 | |
| Boreal evergreen conifer | −35 | −2 | GDD5 350 | | 0.75 | |
| Boreal summergreen | | 5 | GDD5 350 | | 0.65 | |
| Sclerophyll / succulent | 5 | | | | 0.28 | |
| Warm grass / shrub | | | | 22 | 0.18 | |
| Cool grass / shrub | | | GDD5 500 | | 0.33 | |
| Cold grass / shrub | | | GDD0 100 | | 0.33 | |
| Hot desert shrub | | | | 22 | | |
| Cold desert shrub | | | GDD0 100 | | | |

BIOME1 biomes as PFT combinations [R]: tropical rain forest (tropical evergreen), tropical seasonal forest (evergreen + raingreen), tropical dry forest/savanna (raingreen only), broadleaved evergreen/warm mixed forest (warm-temperate evergreen), temperate deciduous (summergreen + cool conifer + boreal summergreen), cool mixed (the same plus boreal conifer), cool conifer, taiga (boreal conifer + boreal summergreen), cold mixed, cold deciduous (boreal summergreen alone — the larch taiga of east Siberia where Tcold < −35), xerophytic woods/scrub (sclerophyll), warm grass/shrub, cool grass/shrub, tundra (cold grass/shrub), hot desert, semi-desert (cold desert shrub), ice/polar desert (nothing survives: GDD0 < 100).

**Holdridge numeric boundaries [R]** (Holdridge 1967; not re-verified): biotemperature class limits 1.5, 3, 6, 12, 24 °C (polar < 1.5, subpolar/alpine 1.5–3, boreal/subalpine 3–6, cool temperate/montane 6–12, warm temperate + subtropical/lower montane-premontane 12–24, tropical > 24; the warm-temperate/subtropical split is the "frost line", roughly biotemperature 16–18 °C). Precipitation classes double: 62.5, 125, 250, 500, 1000, 2000, 4000, 8000 mm. PET (mm) = 58.93 × biotemperature; PET-ratio (PET/P) classes double: 0.125, 0.25, 0.5, 1, 2, 4, 8, 16, 32 (superhumid … superarid). Because PET is a function of biotemperature only, the scheme has **two** real degrees of freedom.

**Whittaker [R/I]**: the 1975 diagram is a hand-drawn figure on MAT (−15…30 °C) × MAP (0…450 cm) with nine polygons; Whittaker gave no numeric boundaries. Digitised polygons exist (the R package `plotbiomes`, digitised from Ricklefs' textbook redraw) and are fine for a validation scatterplot, not as rules. Like Holdridge it has no seasonality, so it cannot separate Mediterranean from monsoon climates of equal MAT/MAP.

**Recommendation [I]**
1. Compute Köppen (Peel) for every land cell — it is exact, uses only monthly T and P, and gives a validation target (area shares, §13).
2. Do NOT map Köppen 1:1 to biomes. Use it as the coarse zone, then run a **BIOME1-style sieve** with (Tcold, GDD5, α or AI, DryM) to choose the vegetation structure. Moisture should be α or AI (which use PET), not raw rainfall: 500 mm is forest at 2 °C MAT and semi-desert at 28 °C.
3. Holdridge is useful for exactly one thing here: its altitudinal belts mirror its latitudinal regions, which is a ready-made rule that a mountain cell uses the *same* classifier with lapse-rate-corrected temperature — the generator gets montane belts for free if temperature is already elevation-corrected (§2).
4. Prefer the LPJ table above for constants (verified) and treat the BIOME1 α thresholds as starting values to tune.

**Köppen → listed biomes mapping [I]** (first guess; the plant-limit rules in §14 refine it)

| Köppen | Default biome | Split rule |
|---|---|---|
| Af | tropical lowland rainforest | peat swamp / mangrove / cloud forest by azonal tests |
| Am | tropical seasonal (monsoon) forest | MAP > ~2500 and DryM60 ≤ 2 → rainforest |
| Aw/As | tropical savanna | MAP > 1500 and fire-protected → seasonal forest; 1000–1500 → dry forest or savanna (fire roll, §3); 500–1000 → savanna / thorn woodland; < 500 → thorn scrub |
| BSh | thorn scrub / thorn woodland / semi-desert steppe | summer-rain → thorn types; winter-rain → Mediterranean scrub edge / shrubland |
| BSk | temperate grassland (steppe) | upper half of the BS band → steppe; lower half → semi-desert steppe |
| BWh | sand desert / hamada-reg / hyper-arid | by AI and substrate (§4) |
| BWk | cold desert | salt flats, badlands by substrate (§4) |
| Csa | Mediterranean woodland (wetter) / Mediterranean scrub (drier) | AI ≥ 0.5 or deep soil → woodland; else scrub |
| Csb | Mediterranean woodland; temperate conifer where MAP high (Pacific-NW type) | MAP > 1400 and Thot < 22 → temperate conifer / temperate rainforest |
| Cfa | warm-temperate evergreen (laurel) forest where Tcold > 5; else temperate deciduous | AI < 0.65 → temperate grassland (pampas) / woodland-savanna |
| Cwa/Cwb | laurel forest / tropical conifer (pine–oak) / subtropical dry forest | Tcold < 15.5 with dry winter and elevation → tropical conifer forest |
| Cfb | temperate deciduous or mixed forest | MAP > 1400 (and summer P ≥ ~10 % of MAP) → temperate rainforest; windy, wet, poor soil → heath and moor |
| Cfc | heath and moor / subpolar woodland | treeline test decides forest vs moor |
| Dfa/Dfb, Dwa/Dwb | temperate deciduous → temperate mixed forest → (colder) taiga | AI < 0.65 → woodland-savanna (aspen parkland); AI < 0.5 → steppe |
| Dsa/Dsb | temperate conifer forest / shrubland | |
| Dfc/Dwc/Dsc | taiga | |
| Dfd/Dwd | taiga (larch, deciduous needleleaf) | |
| ET | arctic tundra (lat) / alpine tundra (elevation) / tropical alpine (|lat| < 23.5°) | polar desert where MAP < 250 mm |
| EF | ice sheet where ice field says so, else polar desert / bare rock | |

### Gaps
- Beck et al. 2018 / 2023 criteria tables could not be opened (login redirect). I believe they are Peel's criteria with one change — s/w decided so the classes are mutually exclusive — but this is unverified.
- Köppen–Trewartha criteria were not retrieved (Wikipedia page says only that Trewartha refined the too-broad C group). [R]: Trewartha splits by months ≥ 10 °C — subtropical C ≥ 8 months, temperate D 4–7, boreal E 1–3, polar F 0 — which matches vegetation better than Köppen's C/D split; verify before use.
- BIOME4 (Kaplan 2003) PFT table (which adds absolute-minimum-temperature limits and NPP-based competition) was not retrieved; the Biome.jl documentation page lists the predictor names (tmin, gdd5, gdd0, twm, snow depth) but no numbers. Box (1981) model limits not retrieved.
- Walter zonobiomes not researched (nine zones, qualitative; nothing to add numerically).

---

## 2. Treeline and altitudinal zonation

### Takeaway
Treeline is a **temperature** line that needs no elevation rule at all: forest is impossible where the growing season (days above 0.9 °C) is shorter than 94 days or its mean temperature is below 6.4 °C. Apply the same lapse-rate-corrected climate rules on mountains and the belts appear by themselves; add separate tests only for cloud forest, tropical alpine, snow line and bare rock.

### Cited Findings
- The TREELIM model predicts global potential treeline from three parameters: a daily-mean threshold of **0.9 °C** defining the growing season, a minimum season length of **94 days**, and a minimum season mean temperature of **6.4 °C**. Körner & Paulsen 2004 had reported 6.7 °C from in-situ loggers. [V-snippet] — [Paulsen & Körner 2014, Alpine Botany 124:1](https://doc.rero.ch/record/324784?ln=fr); [GMBA treeline definition](https://www.gmba.unibe.ch/services/tools/treeline_definition)
- The classic approximation, a warmest-month mean of 10 °C, is the Köppen E boundary (Thot < 10 = polar/tundra). [V] — [Peel et al. 2007](https://hess.copernicus.org/articles/11/1633/2007/hess-11-1633-2007.pdf)
- Treeline elevation by latitude: rises ~75 m per degree of latitude from 70°N to 50°N, ~130 m per degree from 50°N to 30°N, roughly constant at 3,500–4,000 m between 30°N and 20°S. Examples: Abisko 68°N 650 m; Swiss Alps 47°N 2,200 m; Colorado 40°N 3,550 m; Khumbu 28°N 4,200 m; Kilimanjaro 3°S 3,100 m; Sajama, Bolivia 18°S 5,200 m (highest); Torres del Paine 51°S 950 m. [V] — [Wikipedia: Tree line](https://en.wikipedia.org/wiki/Tree_line)
- The permanent snow line lies about **300–1,000 m above treeline** and parallels it. [V] — [Wikipedia: Tree line](https://en.wikipedia.org/wiki/Tree_line)
- Aspect and local effects: in the Northern Hemisphere north-facing slopes have lower treelines (Colorado: 3,550 m vs 3,250 m on NE slopes); snow-filled gullies lowered treeline ~400 m in the Selkirks; Hawaii's treeline (~2,400–2,900 m) sits under a trade-wind inversion (drought, not cold); oceanic coasts have much lower treelines (southern Norway 1,100 m inland vs 500–600 m at the coast). [V] — [Wikipedia: Tree line](https://en.wikipedia.org/wiki/Tree_line)
- The arctic forest–tundra transition zone averages ~145 km wide in NW Canada — the treeline is an ecotone, not a line. [V] — [Wikipedia: Tree line](https://en.wikipedia.org/wiki/Tree_line)
- Cloud forest: the cloud base defines the lower limit of cloud forest; measured cloud bases in Puerto Rico most often 740–964 m (lowest > 600 m); ~1,500 m is taken as the lower limit of upper montane cloud forest in Malaysia; stunted upper montane cloud forest at ~2,550 m in Guatemala; the Massenerhebung effect puts the belt much lower on small/isolated mountains than on large massifs. [V-snippet] — [Van Beusekom et al. (USGS)](https://www.usgs.gov/publications/analyzing-cloud-base-local-and-regional-scales-understand-tropical-montane-cloud); [Holder, fog precipitation in Guatemala](https://uon.sdsu.edu/holder_fog_precipitation.pdf); [Bruijnzeel et al. 2011, Tropical Montane Cloud Forests (CUP)](https://www.cambridge.org/core/product/identifier/CBO9780511778384A035/type/BOOK_PART)

### Inferences
- **Treeline rule [I]**: `forest_possible = GS_len ≥ 94 d AND GS_T ≥ 6.4 °C`. With only monthly means, interpolate the annual temperature curve (a cosine fit through the 12 values) to count days > 0.9 °C. The cheap fallback is Thot ≥ 10 °C; it puts the treeline too low on tropical mountains (no seasons: every month is the "warmest") — there, MAT ≈ 5.5–7 °C is the equivalent.
- **Treeline can also be dry**: on arid mountains the lower forest limit is a moisture line (AI < ~0.3–0.4) and the forest is a band between a dry lower treeline and a cold upper one; in hyper-arid ranges the band closes and there is no forest at all. The same sieve gives this without extra code. [I]
- **Belts [I]** (drop out of the zonal rules applied to lapse-corrected climate; lapse rate ~6.5 °C/km, less, ~5, in humid air): colline = lowland zonal biome; montane = the biome of a zone ~5–10° poleward (mixed/conifer forest); subalpine = conifer/birch up to treeline, with a krummholz ecotone of ~100–200 m; alpine = tundra/meadow from treeline to the snow line; nival = above the snow line (ice where the glacier field has ice, else bare rock and scree).
- **Alpine vs arctic vs tropical alpine [I]**: all are "Thot < 10 / no trees". Arctic tundra if the cell would be treeless at sea level (sea-level-reduced temperature still fails the treeline test); alpine tundra otherwise; **tropical alpine (páramo/puna)** if |latitude| < ~23.5° or annual temperature range (Thot − Tcold) < ~5 °C. Split: páramo AI ≥ 0.65 (wet, northern Andes / East Africa type), puna AI < 0.65 (dry; puna below AI 0.2 becomes high cold desert / salt flat country).
- **Snow line [I]**: use the glacier/ice field directly; where a rule is wanted, the equilibrium line is where annual snowfall equals melt — approx. mean summer (3 warmest months) temperature ≤ 0…+3 °C in wet climates and well below 0 °C in arid ones (dry mountains have snow lines 1,000+ m higher than wet ones at the same latitude, which is why Sajama carries trees at 5,200 m).
- **Bare rock and scree [I]**: slope > ~35–40° (above the angle of repose, soil does not hold) at any elevation, plus everything between the upper limit of closed alpine vegetation (GDD0 < ~100–150) and the ice. Recently deglaciated ground also qualifies if the ice-cover history is available.
- **Cloud forest rule [I]**: tropical/subtropical (Tcold of the sea-level-reduced climate ≥ 15.5–18 °C), humid (AI > 1 or MAP > 1500), windward slope, elevation inside the cloud belt: `cloud_base ≈ 125 m × (T − Tdew)` (lifting condensation level; needs humidity — proxy: lower base nearer the coast and on small islands). Practical bands [R]: ~1,200/1,500–2,500/3,000 m on large continental mountains, 500–1,000 m on small oceanic islands and coastal ranges. A "relief mass" proxy (mean elevation within ~50 km) reproduces the Massenerhebung shift. **Needs a field not listed: relative humidity or cloud-base height**; prevailing wind direction for windward/leeward.
- **Frost hollows [I]**: cells that are local minima with a large upslope contributing area of cold-air drainage get Tcold lowered by 2–5 °C and their treeline inverted (grass in the hollow, trees on the slopes).

### Gaps
- No verified source for the numeric belt limits by latitude (colline/montane/subalpine elevations) — deliberately replaced with the climate-driven rule.
- Cloud-forest elevation bands are recalled (Bruijnzeel 2001; Bruijnzeel et al. 2011), not verified; the book chapters were paywalled.
- Snow-line temperature rule is my simplification; no ELA–climate regression (e.g. Ohmura et al. 1992) was retrieved.

---

## 3. Forest type splits, tropical dry-season rules, and savanna–forest bistability

### Takeaway
Leaf type follows winter cold and season length (verified LPJ limits: tropical ≥ 15.5 °C coldest month; broadleaf evergreen ≥ 3 °C; summergreen −17.7…15.5 °C with GDD5 ≥ 1200; boreal conifer −32.5…−2 °C with GDD5 ≥ 600; larch below that). In the tropics, annual rainfall 1000–2500 mm with a dry season under 7 months is genuinely bistable — fire decides — so the generator should roll a persistent fire state rather than draw a rainfall line.

### Cited Findings
- LPJ limits (table in §1): tropical trees need Tcold ≥ 15.5 °C; temperate broadleaved evergreen Tcold 3.0–18.8 °C and GDD5 ≥ 1200; temperate broadleaved summergreen Tcold −17.7…15.5 °C and GDD5 ≥ 1200; temperate needleleaved evergreen Tcold −2…22 °C and GDD5 ≥ 900; boreal needleleaved evergreen Tcold −32.5…−2 °C and GDD5 ≥ 600; boreal needleleaved summergreen (larch) Tcold −46.5…−5.4 °C, GDD5 ≥ 350; boreal broadleaved summergreen (birch) Tcold ≤ −2 °C, GDD5 ≥ 350. [V] — [LPJmL4 supplement Table 2](https://gmd.copernicus.org/preprints/gmd-2017-145/gmd-2017-145-supplement.pdf)
- Köppen's tropical moisture split: Af every month ≥ 60 mm; Am driest month < 60 mm but ≥ 100 − MAP/25; Aw below that. [V] — [Peel et al. 2007](https://hess.copernicus.org/articles/11/1633/2007/hess-11-1633-2007.pdf)
- Staver, Archibald & Levin 2011: "at intermediate rainfall (1000 to 2500 mm) with mild seasonality (less than 7 months), tree cover is bimodal, and only fire differentiates between savanna and forest"; these are alternative stable states over large areas including parts of Amazonia and the Congo; transitions are "neither smooth nor easily reversible". [V-snippet, abstract] — [Staver et al. 2011, Science 334:230](https://collaborate.princeton.edu/en/publications/the-global-extent-and-determinants-of-savanna-and-forest-as-alter/)
- Hirota et al. 2011: tropical tree cover falls into three states — forest (~80 % cover), savanna (~20 %), treeless (~5 %) — with rare intermediate cover (around 60 % and around 5 %); a commentary summarises forests (> 60 % cover) mainly above 2500 mm/yr and savannas (5–50 %) most common between 750 and 2000 mm/yr. [V-snippet; secondary summaries, not the paper's own tables] — [Hirota et al. 2011, Science 334:232](https://landscapepartnership.org/maps-data/climate-context/cc-resources/ClimateSciPDFs/Science2011Hirota2325.pdf); [Phys.org summary](https://phys.org/news/2011-10-forest-savanna-quickly.html)
- A press summary gives a 40–45 % tree-cover tipping point below which fire spreads readily; the search could not confirm it in the paper. [V-snippet, low confidence] — [Phys.org](https://phys.org/news/2011-10-forest-savanna-quickly.html)

### Inferences
**Leaf-type decision table [I, built on the LPJ/BIOME1 limits]** — applies where the treeline test passes and moisture allows closed forest (AI ≥ ~0.65 or α ≥ 0.65):

| Condition | Forest biome |
|---|---|
| Tcold ≥ 18 (or ≥ 15.5), DryM60 ≤ 2, MAP ≥ 2000 | tropical lowland rainforest |
| Tcold ≥ 15.5, DryM60 3–5, MAP 1500–2500 | tropical seasonal (monsoon) forest |
| Tcold ≥ 15.5, DryM60 5–8, MAP 700/1000–1500/2000 | tropical dry forest (if not burnt to savanna) |
| Tcold ≥ 15.5, DryM60 ≥ 8, MAP 400–800 | thorn woodland |
| Tcold 5…15.5, humid all year, Thot ≥ 22 | warm-temperate evergreen (laurel) forest |
| Tcold 5…15.5, dry winter, elevation or poor soil | tropical conifer (pine/pine–oak) forest |
| Tcold > 0…5 (or more), dry summer (Cs) | Mediterranean woodland |
| Tcold −2…10, MAP ≥ 1400, Thot < 22, no summer drought (≥ ~10 % of MAP in the 3 summer months; fog can substitute) | temperate rainforest |
| Tcold −15…5, GDD5 ≥ 1200, summer rain | temperate deciduous broadleaf forest |
| Tcold −19…−2, GDD5 900–1500 (both summergreen and conifer survive) | temperate mixed forest |
| Tcold −2…5 with dry summers (Csb/Dsb), or continental mountains GDD5 900–1200 | temperate conifer forest |
| Tcold −35…−2 (LPJ −32.5), GDD5 350/600–900/1200 | taiga (evergreen) |
| Tcold < −35 | taiga (larch, deciduous needleleaf) |
| GDD5 < 350 or treeline test fails | tundra |

- The −15 °C limit for broadleaf summergreens and the absence of conifers in warm humid lowlands are why "mixed" appears as a **band** between deciduous and taiga: implement mixed forest as "both PFTs pass the sieve", not as its own climate box. [I]
- Summer drought (Cs/Ds, Psdry < 40 mm) favours evergreen needleleaf and sclerophyll over summergreen broadleaf — the reason the Pacific Northwest is conifer and the same latitudes of eastern North America are deciduous. [I]
- **Tropical dry-season counts [R]**: commonly used bands are rainforest 0–3 months < 100 mm (or < 60 mm), seasonal/semi-evergreen 3–5, dry deciduous 5–8, thorn 8–10, desert > 10; tropical dry forest is conventionally MAP 250–2000 mm with PET/P > 1 (Murphy & Lugo 1986). Not re-verified.
- **Simple fire/herbivory model for a generator [I]**:
  1. Compute `fuel = grass productivity ≈ f(wet-season rain)` and `dryness = DryM60` (a fire season needs both: enough rain to grow fuel, and a dry season to cure it). Fire likelihood peaks at MAP ~700–1800 mm with DryM60 ≥ 3–4; it is fuel-limited below ~400–500 mm and moisture-limited above ~2500 mm.
  2. In the bistable window (MAP 1000–2500 mm, DryM60 < 7; outside the tropics ignore), draw the state from **low-frequency noise** (patch scale 50–300 km) biased by: rainfall (probability of forest rising from ~0.1 at 1000 mm to ~0.9 at 2500 mm), soil fertility/texture (sandy, poor soils → savanna), topographic fire refuges (steep, dissected terrain and wind-shadow sides of rivers and lakes → forest), ignition sources (settlement proximity from the history sim, lightning ∝ convective rain).
  3. Apply hysteresis for natural-looking shapes: a cell adjacent to forest has a lower burn chance (forests shade out grass), one adjacent to savanna a higher one; run 2–4 cellular-automaton smoothing passes. That produces sharp forest–savanna edges with gallery forests and forest islands, which is the observed pattern (cover is bimodal, edges are abrupt).
  4. Below 1000 mm: savanna → thorn woodland → thorn scrub by rainfall (tree cover capped roughly linearly by MAP up to ~650 mm — Sankaran et al. 2005 [R]). Above 2500 mm or DryM60 ≤ 2: forest regardless.
  5. Herbivory: megaherbivore presence (from the fauna layer, if any) lowers tree cover in the 400–1000 mm band; cheap proxy = extra savanna bias on fertile (volcanic, alluvial) soils, where grazers concentrate.
- The same fire logic, weaker, gives **temperate woodland-savanna** (oak savanna, aspen parkland, forest-steppe): AI 0.5–0.75 between steppe and forest, with a noise-driven patch state, and **Mediterranean woodland vs scrub** (frequent fire and thin soil → scrub). [I]

### Gaps
- The exact Staver/Hirota probability curves (tree-cover probability versus MAP) were not extracted; the ramp in step 2 is my own.
- Dry-season-length bands for tropical forest types are recalled, with no verified primary source this session.
- Sankaran et al. 2005 (650 mm cap on woody cover in African savannas) is recalled, unverified.

---

## 4. Deserts and drylands

### Takeaway
Use the UNEP aridity index for the dryness classes (hyper-arid < 0.05, arid 0.05–0.20, semi-arid 0.20–0.50, dry sub-humid 0.50–0.65) and decide the *kind* of desert surface from geomorphology — sediment sinks downwind get sand, uplands and plains get rock/gravel, soft rock with relief gets badlands, closed-basin floors get salt.

### Cited Findings
- UNEP (1992) aridity index AI = P/PET: hyper-arid < 0.05 (7.5 % of global land), arid 0.05–0.20 (12.1 %), semi-arid 0.20–0.50 (17.7 %), dry sub-humid 0.50–0.65 (9.9 %) — drylands total 47.2 % of land. [V] — [Wikipedia: Aridity index](https://en.wikipedia.org/wiki/Aridity_index)
- Köppen B: desert MAP < 5 × Pthreshold, steppe 5–10 × Pthreshold; hot/cold split at MAT 18 °C; BWh is the single largest climate type at 14.2 % of land. [V] — [Peel et al. 2007](https://hess.copernicus.org/articles/11/1633/2007/hess-11-1633-2007.pdf)
- Köppen has an optional "n" modifier for B climates with frequent fog (coastal fog deserts). [V] — [Wikipedia: Köppen climate classification](https://en.wikipedia.org/wiki/K%C3%B6ppen_climate_classification)
- In Ellis's anthrome data, bare earth covers nearly one third of rangeland anthromes — deserts are mostly used as rangeland, relevant for land-use assignment. [V] — [Wikipedia: Anthropogenic biome](https://en.wikipedia.org/wiki/Anthropogenic_biome)

### Inferences
**Dryness → biome [I]**

| AI | MAT / Tcold | Biome |
|---|---|---|
| < 0.03–0.05 (MAP typically < 25–50 mm) | any | hyper-arid desert (no perennial plants outside wadis) |
| 0.05–0.20 | MAT ≥ 18 | hot desert: sand desert / rocky wasteland / badlands / salt flat by substrate |
| 0.05–0.20 | MAT < 18, Tcold < 0 | cold desert |
| 0.20–0.35 | any | semi-desert steppe (sparse bunchgrass and dwarf shrub, 10–30 % cover) |
| 0.20–0.50 | Tcold ≥ 15.5, summer rain | thorn scrub (drier) / thorn woodland (wetter) |
| 0.35–0.65 | Tcold < 15.5 | temperate grassland (steppe/prairie); shrubland where rain comes in winter or soils are thin |
| 0.50–0.65 | | woodland-savanna / dry forest / Mediterranean types |

**Surface type of a desert cell [I; conventional geomorphology, share figures [R]]** — roughly 20–25 % of Earth's deserts are sand, the rest rock, gravel, and bare soil, so sand desert must be the *minority*:

| Biome | Conditions (from listed fields) |
|---|---|
| Sand desert (erg) | topographic low / basin or plain downwind of a sediment source (dry river fans, old lake beds, sandy coasts, sandstone/granite uplands); low slope (< ~2–3°); AI < 0.2 (active dunes need MAP < ~150 mm; 150–400 mm gives fixed, vegetated dunes → semi-desert). Compute a "sand supply" field: accumulate eroded sediment from river discharge history × arid deflation, transport it along prevailing wind, deposit where wind converges or relief blocks it. **Needs: prevailing wind direction** (not in the list). |
| Rocky wasteland (hamada / reg) | the default arid surface: plateaus and pediments (hamada = bare bedrock on uplands with resistant rock; reg/serir = gravel plains, lag left where wind removed the fines). Slope low–moderate, no sediment sink, resistant rock type. |
| Badlands | soft, poorly cemented sedimentary rock (shale, mudstone, marl, tuff, loess) + local relief/base-level fall (slope 10–40°, high drainage density) + semi-arid, intense storm rainfall (AI ~0.1–0.5, MAP ~200–600 mm): enough rain to erode, too little for plant cover. |
| Salt flat / playa | floor of an endorheic (closed) basin — a flow-accumulation sink with no outlet — where annual PET > inflow, so the lake is dry most of the time; slope ≈ 0; salinity high. Ringed by a salt/soda lake or ephemeral lake in wetter years. |
| Hyper-arid desert | AI < 0.05 regardless of surface (surface variant can still be stored as a secondary tag). |
| Cold desert | AI < 0.2 and (MAT < 18, Tcold < 0); continental interiors and rain shadows, high plateaus. |
| Polar desert | Thot < 10 and MAP < 250 mm, not ice-covered. |
| Lava field | volcanism field high + young surface (recent eruption in the history sim): bare basalt at any climate; recolonisation time ~10² yr wet-tropical to 10³–10⁴ yr arid/cold — pick by `age × AI`. **Needs: lava age / last-eruption time.** |
| Fog desert (variant of hyper-arid) | coastal cells (distance to coast < ~50 km, elevation < ~1000 m) on a west coast with a cold eastern-boundary current: SST much colder than the zonal mean (SST anomaly ≤ −3 °C) under subtropical high pressure. Gives lichen/"lomas" patches at the fog belt (300–1,000 m). |

- Cold vs hot desert matters for flora (succulents and tall cacti only where frost is rare: Tcold > ~5 °C; sagebrush/saltbush shrub steppe below). [I]
- Rain-shadow, continental-interior and subtropical-high deserts all fall out of a decent climate simulation; no desert-specific placement rule is needed beyond AI. [I]

### Gaps
- No primary source was fetched for the erg/reg proportions or the 150 mm active-dune limit; both are recalled.
- No verified numeric criteria for badland formation (lithology/slope thresholds are my estimates).
- Fog-desert thresholds are my inference.

---

## 5. Wetlands (azonal) and mangroves

### Takeaway
Wetlands are placed by **water, not climate**: water table at or near the surface for a long part of the year. Climate then picks the type — peat needs a positive water balance or permanent waterlogging; trees versus herbs splits swamp from marsh; water source (rain-only versus groundwater) splits bog from fen at roughly pH 5–5.5; salt plus tide gives salt marsh, or mangrove where the coldest month is warm (≈ 20 °C winter SST, frost-free).

### Cited Findings
- Bog = ombrogenous mire, raised above its surroundings, fed only by precipitation; fen = geogenous mire in a depression, fed by water that has touched mineral soil or bedrock. Poor fens typically pH < ~5.5, rich fens > ~5.5; another review gives bogs generally pH < 5.0 and fens > 6.0; the bog/fen limit is not sharp in vegetation or chemistry. [V-snippet] — [Irish Peatland Conservation Council glossary](https://www.ipcc.ie/a-to-z-peatlands/peatland-glossary/); [Wheeler & Proctor / Hájek et al. 2006 review](https://cbrb.sav.sk/usr/Dano/docs/Persp_in_Plant_Ecol_2006.pdf)
- A histosol (peat soil) is defined by at least 12–18 % organic carbon and thickness ≥ 40 cm. [V-snippet] — [International Peatland Society: types of peatlands](https://peatlands.org/peatlands/types-of-peatlands/)
- Fens keep a higher, steadier water table because groundwater supplements them; runoff from bogs may cease in dry periods; the lagg (fen/swamp) around a raised bog has a strongly fluctuating water table. [V-snippet] — [Rochefort et al. 2012, Northern Peatlands](https://www.gret-perg.ulaval.ca/fileadmin/Fichiers/centre_recherche/Rochefort_etal_NortherPeatlands-chapter09_2012_02.pdf)
- Mangroves: classical limit = mean winter SST ≥ 20 °C (Duke et al. 1998) or coldest-month mean air temperature 20 °C (Walsh 1974), with exceptions; winter SST at actual range limits spans 22 °C (southern Japan) down to 12.5 °C (eastern Australia); in humid regions (New Zealand, E Australia) mangroves persist at mean winter air temperatures < 8 °C, at arid limits (Mauritania, Peru) only > 20 °C. [V-snippet] — [Quisthoudt et al. 2012, Trees](https://www.africamuseum.be/publication_docs/Quisthoudt%20et%20al%202012%20Temperature%20variation%20among%20mangrove%20-%20Trees.pdf); [Osland et al., mangrove range limits (USGS)](https://www.usgs.gov/publications/mangrove-expansion-and-contraction-a-poleward-range-limit-climate-extremes-and-land)
- The mangrove/salt-marsh boundary in the SE USA is set by extreme freezes: mangrove dieback below a threshold zone of **−6.3 to −7.6 °C** air temperature; fewer/weaker frosts → mangroves replace salt marsh. [V-snippet] — [Osland et al. (USGS)](https://www.usgs.gov/publications/mangrove-expansion-and-contraction-a-poleward-range-limit-climate-extremes-and-land); [Saintilan et al. 2014](https://typeset.io/pdf/mangrove-expansion-and-salt-marsh-decline-at-mangrove-4uoy2pl8g8.pdf)
- Azgaar's generator assigns "wetland" by wetness + elevation only (moisture > 40 near sea level, or > 24 at mid elevation, and temperature > −2 °C) — an example of how crude game rules are. [V-snippet] — [Azgaar FMG biomes.js (mirror)](https://git.korz.dev/backups/Fantasy-Map-Generator/src/tag/v1.99/modules/biomes.js)

### Inferences
**Wetness index [I]**: prefer the simulated water-table depth. Where only terrain is available use the topographic wetness index `TWI = ln(a / tan β)` (a = upslope contributing area per unit contour width, β = slope); weight a by local runoff (P − AET) so a big catchment in a desert does not count. Wetland candidates: water table within ~0.3 m of the surface for ≥ ~2–4 months of the growing season, or surface inundation.

**Wetland decision table [I]** (first match wins; "saline" = salinity field high or tidal)

| Biome | Rule |
|---|---|
| Mangrove swamp | intertidal (elevation between low and high tide; use −1…+2 m and tidal range), sheltered (low wave exposure: bays, estuaries, deltas, lee of reefs), soft sediment, SSTmin ≥ 20 °C (relax to ≥ 16 °C on humid coasts, AI > 0.65) **and** coldest-month air T ≥ ~13–16 °C / no hard frost. On arid coasts require fresh-water input (river mouth) or keep only as a fringe. Typical limit ≈ 25–30° latitude; ~38°S in humid Australasia. |
| Salt marsh | same setting, colder (fails the mangrove temperature test), or the upper intertidal behind mangroves on arid tropical coasts (hypersaline flats = salt flat instead). |
| Mudflat | intertidal, sheltered, fine sediment, below the vegetation limit (lower half of the tidal frame); wide where tidal range is large (> 4 m macrotidal) and slope tiny. |
| River delta | river discharge above a threshold meets sea or lake; low-slope depositional fan; wave/tide energy low relative to sediment load. A container biome: its cells are a mosaic of marsh/swamp/mangrove/floodplain/channels. |
| Tropical peat swamp forest | Tcold ≥ 18, DryM60 ≤ 1–2 and MAP ≥ ~2000–2500 (P > PET every month), flat lowland interfluve or coastal plain with very low gradient, no mineral sediment input (away from active floodplain levees); builds rain-fed domes. |
| Peat bog | P > PET in most months (annual AI ≥ ~1.0–1.2), cool: Thot < ~17–20 °C (decay slow), flat or gently sloping, acid rock (granite, sandstone) or already-raised peat; rain-fed. Blanket bog (covers slopes up to ~15–20°) where AI > ~2 with ≥ ~160 rain days and Thot < 15 °C (hyper-oceanic). Permafrost variants (palsa, polygon mires) in ET/Dfc. |
| Fen | waterlogged all year, groundwater- or stream-fed (valley floor, spring line, lake margin, toe of slope), base-rich rock (limestone, basalt) → rich fen (pH > 5.5); poor fen on acid rock; cool to temperate. |
| Swamp (forested wetland on mineral soil) | wetland candidate + trees can grow (treeline test passes, not saline) + water is seasonal or shallow (< ~0.5–1 m, with oxygenated flow or periodic drawdown): floodplain backswamps, lake margins. Climate decides cypress/alder/várzea type. |
| Marsh (herbaceous, mineral soil) | wetland candidate, water too deep or too persistent for trees (standing 0.2–1.5 m most of the year), or frequent disturbance (fire, ice, flood scour), or nutrient-rich lake/river margin (reed beds). |
| Flooded grassland | large flat basin (inland delta, floodplain depression) with strongly seasonal flooding (inundated ~3–8 months) in a tropical/subtropical seasonal climate (Aw, Cwa): Pantanal, Okavango, Sudd, Everglades. Trees excluded by flood-then-fire alternation. |
| Seasonal floodplain | cells within the river's flood reach (height above the channel < flood stage, which scales with discharge variability), inundated < ~3 months; riparian forest / wet meadow by climate; the most fertile soil class (alluvium) — prime cropland. |

- Tree vs herb in wetlands follows hydroperiod more than climate; a simple proxy is `flood_depth × flood_duration`: low → swamp forest, high → marsh, permanent deep → open water. [I]
- Peat accumulation exists in two regimes [I, consistent with the cited definitions]: (a) cool and wet (boreal/oceanic), where cold slows decay; (b) hot and permanently wet (equatorial peat domes), where year-round waterlogging does. Seasonally dry tropical wetlands do not build peat. Permafrost also impedes drainage, which is why wetlands are so extensive on the flat boreal/low-arctic plains at modest rainfall.

### Gaps
- No verified numeric hydroperiod classes (e.g. Cowardin, Ramsar) were retrieved; all inundation durations above are my estimates.
- No verified climate envelope for peat (the "P > PET, cool" rule is the textbook statement; numeric limits such as the PEATSTASH/Gallego-Sala blanket-bog envelope are recalled approximately — [R]: MAT of the warmest month < 14.5 °C, P/PET-type moisture index ≥ 2.1, mean annual T > −1 °C — verify).
- IUCN Global Ecosystem Typology (Keith et al. 2022) profiles — which give drivers for each wetland functional group — were not opened.

---

## 6. Coastal and marine biomes

### Takeaway
Marine biomes are a depth × temperature × substrate table: shelf to 200 m, reefs where the coldest month stays ≥ 18 °C in clear shallow water, kelp on cold nutrient-rich rock, seagrass on sheltered shallow sediment down to ~11 % of surface light, sea ice where the surface reaches the freezing point of sea water. Shore type follows wave exposure, slope and sediment supply.

### Cited Findings
- Kleypas et al. 1999 compiled temperature, salinity, light, aragonite saturation and nutrients for ~1,000 reef locations; temperature and salinity limits are close to earlier ones; no clear limiting nutrient level emerged at that scale; low aragonite saturation and low light at high latitudes co-limit reefs along with temperature. [V-snippet, abstract only] — [Kleypas, McManus & Meñez 1999, Am. Zool. 39:146](https://academic.oup.com/icb/article/39/1/146/124568)
- A later reef-habitat study gives present-day reef waters as 21.7–29.6 °C, salinity 28.7–40.4 psu, minimum aragonite saturation 3.28 (falling to 2.82), mean minimum light 450 µmol photons m⁻² s⁻¹. [V-snippet] — [Guan et al. 2015, PLoS ONE (ReefHab)](https://pmc.ncbi.nlm.nih.gov/articles/PMC4452591)
- Kelp: temperate and polar coastal waters on hard substrate; needs high nitrogen/phosphorus; most productive in upwelling zones; minimum annual light dose above ~50 E m⁻² yr⁻¹; absent from warm, nutrient-poor tropical surface water but predicted (and found, e.g. Galápagos) in deep cool tropical water down to ~200 m. [V] — [Wikipedia: Kelp forest](https://en.wikipedia.org/wiki/Kelp_forest)
- Seagrass extends to the depth receiving on average **11 % of surface irradiance** (Duarte 1991); reported range ~5–25 % (up to > 30 % in turbid water). [V-snippet] — [Light requirements of marine rooted macrophytes (Aarhus Univ. DCE)](https://dce.au.dk/fileadmin/dce.au.dk/Udgivelser/Notater_2018/Light_requirements_of_marine_rooted_macrophytes.pdf); [Ralph et al. 2007, JEMBE](https://www.chesapeake.org/stac/downloads/habitat%20STAC/ralph%20et%20al%202007%20JEMBE.pdf)
- Mangrove limits: see §5.

### Inferences
**Kleypas 1999 reef limits [R, not re-verified]**: weekly SST 16.0–34.4 °C (mean 27.6); reefs essentially absent where the long-term coldest-month SST is below ~18 °C; salinity 23.3–41.8 (mean 34.3); nitrate 0–3.34 µmol/L, phosphate 0–0.54 µmol/L; aragonite saturation 3.28–4.06 (mean 3.83); depth of adequate light penetration mean ~53 m (9–81 m).

**Marine / coastal decision table [I]** — depth in m below sea level, SST in °C; "light depth" z_light = ln(1/f) / Kd where Kd (attenuation) rises with turbidity: use Kd ≈ 0.04 (clear ocean) … 0.5+ (river plume); turbidity proxy = nearby river sediment discharge.

| Biome | Rule |
|---|---|
| Sea ice | SSTmin ≤ −1.8 °C (freezing point at salinity 35; `Tf ≈ −0.054 × S`). Permanent pack where SSTmax also ≤ ~0 °C; seasonal ice otherwise (store ice months). |
| Coral reef | SSTmin ≥ 18 °C, SSTmax ≤ ~30–31 °C, depth < 30–50 m (mesophotic to ~100 m in very clear water), salinity 30–40, hard or stable substrate, low turbidity/nutrients: exclude cells within the plume of large rivers (discharge-scaled radius; e.g. the Amazon/Ganges gaps) and strong upwelling zones (cold + nutrient-rich). More reef on the west side of ocean basins (warm currents) — falls out of SST. Types by setting: fringing (on the coast), barrier (shelf edge), atoll (subsided volcanic island: volcanism history + shallow summit). |
| Kelp forest | rocky bottom (steep/exposed coast, resistant rock), depth < 30–40 m (to light limit), SSTmax ≤ ~20 °C (warm-month mean; some species to ~23 °C), nutrient-rich (upwelling coasts, high latitudes). |
| Seagrass meadow | soft sediment (sand/mud), sheltered (lagoon, bay, lee of reef/island), depth < z(11 % light) — ~1–5 m in turbid estuaries, 20–40 m in clear tropical or Mediterranean water; SST from ~0 to 35 °C (all but polar); salinity > ~5. |
| Estuary / lagoon | river mouth with tidal mixing (salinity gradient 0.5–30) or water body behind a barrier with restricted exchange; depth < ~10–20 m. Hypersaline lagoon where AI < 0.2 and no river. |
| Mudflat | see §5. |
| Sandy shore and dunes | wave-exposed to moderately exposed coast with sand supply (river sediment discharge nearby and alongshore drift, sandy hinterland, glacial deposits), low nearshore slope; dunes behind where onshore wind and a dry season or wide beach exist. Barrier islands on low-gradient, sediment-rich, micro- to mesotidal (< 4 m) coasts. |
| Rocky shore | steep coast (coastal slope > ~10–15° or cliff), resistant rock, high wave exposure (long fetch), low sediment supply; also young volcanic and glaciated (fjord) coasts. |
| Coastal scrub / grassland | first ~0.1–5 km inland on exposed coasts: salt spray + wind prune trees; strongest on windward temperate/subpolar coasts (machair, coastal prairie, strandveld) and on fog coasts. |
| Shelf sea | depth 0–200 m beyond the shore zone (the shelf break averages ~130–200 m). |
| Open ocean (pelagic over slope/deep water) | depth > 200 m, ice-free; optionally sub-tag by productivity: upwelling (eastern boundaries, equator, subpolar) vs oligotrophic gyres (|lat| 15–35° centres). |
| Abyssal plain | sea floor at 3,000–6,000 m with low slope (sediment-smoothed). The continental slope/rise (200–3,000 m) and mid-ocean ridge flanks are separate floor types if wanted. |
| Trench | sea floor deeper than 6,000 m (hadal), at subduction boundaries — from the tectonic model. |
| Hydrothermal vents | on spreading ridges and back-arc basins (and volcanic arcs/hotspot seamounts): volcanism field high on the sea floor; typical depths ~2,000–4,000 m on ridges. Sparse point features (one field per ~10–100 km of ridge), not area biomes. |
| Cold seeps | continental margins: thick organic sediment (off big deltas), subduction accretionary wedges, salt-tectonic basins; 200–3,000+ m. Point features. |
| Sea caves | rocky shore + wave exposure + jointed or soluble rock (limestone, basalt, sandstone) at sea level — see §7. |

- Wave exposure can be computed without a wave model as **fetch**: the open-water distance from the coast cell in the 8–16 compass directions, weighted toward the prevailing wind. [I]
- Tidal range matters for mudflat/salt-marsh/mangrove width and is not in the field list; a cheap proxy is "funnel-shaped bays and shallow shelf seas amplify, open ocean coasts and enclosed seas damp". **Needs: tidal range (or the proxy).** [I]

### Gaps
- Kleypas 1999's table could not be opened; the [R] numbers need checking (the 3.28 aragonite minimum is corroborated by Guan et al. 2015).
- No verified numeric temperature limit for kelp was found this session (the common "< 20 °C" figure is recalled).
- Depth-zone boundaries (200 m shelf, 3,000–6,000 m abyssal, > 6,000 m hadal) are standard oceanographic conventions, recalled, not fetched.
- Aragonite saturation is not among the listed fields; it correlates with SST closely enough that the SSTmin ≥ 18 °C rule covers it for an Earth-like ocean.

---

## 7. Fresh water and underground

### Takeaway
Classify rivers by a flow-regime tag derived from discharge seasonality and winter temperature, and lakes by basin water balance (open, closed/saline, ephemeral). Caves need soluble rock plus water; aquifers need porous rock plus recharge; oases are where an aquifer or an exotic river meets a desert surface.

### Cited Findings
- No primary source specific to this question was retrieved this session. The only cited anchor is Köppen/UNEP dryness for the climate side: endorheic and ephemeral waters belong to B climates / AI < 0.5 (thresholds in §1 and §4). — [Peel et al. 2007](https://hess.copernicus.org/articles/11/1633/2007/hess-11-1633-2007.pdf); [Wikipedia: Aridity index](https://en.wikipedia.org/wiki/Aridity_index)

### Inferences (all [I], textbook hydrology/geomorphology; thresholds are starting values)

**River reach types** (per river cell; Q = mean discharge, Qmin/Qmax monthly)

| Type | Rule |
|---|---|
| Upland stream | channel slope > ~0.5–1 % (steep, coarse bed, rapids/falls where slope > ~4 %); small Q; cold, oxygen-rich. |
| Lowland river | slope < ~0.05–0.1 %, large Q; meanders, levees, oxbows; carries a floodplain biome beside it. |
| Seasonal river | flows every year but Qmin/Q < ~0.05–0.1 (dry-season pools only): Aw, Cs, BS climates, or snowmelt-fed. |
| Episodic arid (wadi/arroyo) | local AI < 0.2 and the catchment generates no base flow; flows only after storms (flag: flow months < 2). An *exotic* river (Nile type) is the exception: perennial through desert because the headwaters are humid — decide from routed discharge, never from local climate. |
| Freeze-thaw river | Tcold < ~−5…−10 °C for ≥ 3–4 months: ice cover in winter, spring-melt flood peak, ice-jam flooding; in permafrost, braided and with near-zero winter flow. |

**Lakes** (from depression filling in the hydrology model)

| Type | Rule |
|---|---|
| Lake (fresh, open) | basin fills to its spill point: inflow + P > evaporation; has an outlet. |
| Salt / soda lake | terminal lake of a closed basin: equilibrium area where `inflow = (E − P) × area`; permanent water. Soda (alkaline carbonate) where the catchment is volcanic (volcanism/rock-type field: basalt, trachyte — East African Rift type); chloride/sulphate salt otherwise. |
| Seasonal / ephemeral lake | closed basin whose equilibrium area collapses in the dry season or most years (AI < ~0.2–0.3); also pans, vernal pools, and tundra thaw ponds. Dry state = salt flat/playa (§4). |
| Geothermal pools | volcanism/heat-flow field high + groundwater present (recharge > 0 or a water table) → hot springs, geysers, fumaroles; point features clustered along rifts, arcs and hotspots. |
| Oasis and springs | desert cell (AI < 0.2) where the water table reaches within ~0–5 m of the surface: basin lows fed by a regional aquifer, fault lines and escarpment toes, the contact of permeable over impermeable rock, alluvial fans at mountain fronts, or the banks of an exotic river. Point/patch features with riparian or palm-grove vegetation. |

**Underground**

| Type | Rule |
|---|---|
| Dry caves | soluble rock (limestone, dolomite, marble, gypsum, salt) above the present water table — formed when it was wetter or when base level was higher (uplift, incised valleys); also lava tubes in basalt (volcanism), and talus/tectonic caves. In arid karst, caves are relict and dry. |
| Underground streams / pools | karst in a humid climate: carbonate rock + MAP > ~500–600 mm (dissolution scales with runoff P − AET and soil CO₂, so warm-wet is fastest) + relief (hydraulic gradient). Surface signs: sinkholes, disappearing streams, few surface rivers, springs at the base — reduce surface drainage density on karst cells. |
| Groundwater / aquifer | porous or fractured rock (sandstone, gravel/alluvium, chalk, fractured basalt, karst limestone) with recharge (P − AET > 0 now or in the past: "fossil" aquifers under deserts) and a confining layer below (shale, clay, crystalline basement). Poor aquifers: unfractured granite/gneiss, shale. This is the Dwarf Fortress "aquifer layer" made geological. |
| Sea caves | coastal cliff at sea level with wave exposure and jointed/soluble rock; flooded ("blue hole", anchialine) caves where karst meets a sea level that has risen. |

### Gaps
- No primary references were fetched for river-regime or lake classifications (e.g. Köppen-style river regimes of Pardé/Haines, GLWD lake types) or for karst rainfall thresholds; every threshold in this section is my estimate.
- Karst and aquifers need a **rock-type field that distinguishes carbonate/evaporite from other sedimentary rock, plus porosity/permeability** — confirm the planned "rock type" field carries that.

---

## 8. Soil as an intermediate layer

### Takeaway
A small soil model is worth having: three or four derived fields (depth, texture, fertility/pH, drainage/organic content) from the CLORPT factors resolve several otherwise arbitrary biome splits (heath vs forest, bog vs fen, savanna vs forest, scrub vs woodland) and directly serve farming gameplay.

### Cited Findings
- BIOME1's title and design include "soil properties": the moisture index α is computed with a soil water-holding capacity, so soil texture already enters the best-known mechanistic biome model. [V-snippet] — [Prentice et al. 1992](https://pure.iiasa.ac.at/id/file/9128)
- Histosols (peat soils): ≥ 12–18 % organic C and ≥ 40 cm thick. [V-snippet] — [International Peatland Society](https://peatlands.org/peatlands/types-of-peatlands/)
- Staver et al. extend their analysis to include soils alongside rainfall, seasonality and fire as savanna/forest determinants. [V-snippet] — [Staver et al. 2011](https://collaborate.princeton.edu/en/publications/the-global-extent-and-determinants-of-savanna-and-forest-as-alter/)

### Inferences (all [I]; soil-order/climate associations are standard pedology, recalled)
- **Jenny's state factors**: soil = f(CLimate, Organisms, Relief, Parent material, Time). Per-cell model:
  - `depth` ∝ weathering rate (rises with T and P − AET) × time ÷ erosion (slope): thin on slopes > ~25°, ridges, young lava, recently glaciated rock; thick on footslopes, valley floors, loess and alluvium.
  - `texture` from parent rock: granite/sandstone → sandy, acid; shale/mudstone → clay; limestone → thin, clay-rich, base-rich; basalt/volcanic ash → loamy, fertile; loess/alluvium/glacial till → silt-loam, the best farmland.
  - `fertility` = base supply (rock) × leaching penalty (strong where P ≫ PET and hot) × organic matter (highest in cool or semi-arid grassland) + bonuses for fresh inputs (floodplain silt, volcanic ash, loess).
  - `drainage` = texture × slope × water-table depth — replaces the Dwarf Fortress "drainage" value.
- **Soil order from climate + parent material** (USDA order ≈ WRB group): ice/permafrost → Gelisols (Cryosols); peat → Histosols; AI < 0.2 → Aridisols (Calcisols/Gypsisols; Solonchaks in salt basins); steppe AI 0.2–0.65 under grass → Mollisols (Chernozems/Kastanozems — deep, black, the most fertile zonal soil); boreal conifer on sand, cool-humid → Spodosols (Podzols — acid, poor); temperate deciduous forest → Alfisols (Luvisols — good farmland); warm-humid, old surfaces → Ultisols (Acrisols — leached, poor); hot-wet, very old surfaces → Oxisols (Ferralsols — very poor; nutrients are in the biomass); swelling clay in seasonal tropics on basalt/alluvium → Vertisols; volcanic ash → Andisols (Andosols — fertile, high water holding); young/steep/floodplain → Entisols/Inceptisols (Fluvisols on floodplains).
- **Biome effects**: nutrient-poor sand or acid rock in a wet-cool climate → heath/moor instead of forest; serpentine/ultramafic or thin limestone soil → scrub instead of forest; white-sand soils in rainforest climate → stunted "heath forest"; poor sandy soil in the bistable band → savanna (cerrado); deep fertile loess in the forest–steppe ecotone → grassland; shallow soil lowers the water bucket and so lowers α (drier biome on rocky slopes).
- **Farming gameplay**: yield ≈ climate suitability (GDD above crop base, growing-season water = α or irrigation) × soil fertility × workability (slope < ~8–15°, depth > ~0.5 m, not waterlogged, not saline). This is also the suitability score that land-use assignment needs (§9).

### Gaps
- No pedology source was fetched (Zobler 1986 soil units, Jenny 1941, USDA Soil Taxonomy or WRB keys); order–climate associations are recalled general knowledge and should be checked before becoming rules.
- Whether the planned "rock type" field is detailed enough (felsic/mafic/carbonate/clastic grain size/ash) decides how much of this is possible.

---

## 9. Human land use (anthromes)

### Takeaway
Anthromes are assigned from **population density first, then dominant land use** — a rule shape that maps directly onto a history simulation: the sim supplies population per cell, the climate/soil suitability score decides whether used land is crop, pasture or plantation.

### Cited Findings
- Anthromes (Ellis & Ramankutty 2008) group into dense settlements, villages, croplands, rangelands, forested (later "seminatural"/"cultured") lands and wildlands, along a population-density gradient from settlements to wildlands; settlement classes include agricultural settlements above **100 persons/km²**. [V] — [NASA Earth Observatory: Human ecosystems](https://earthobservatory.nasa.gov/IOTD/view.php?id=40554)
- Dense settlements never fall below 100 persons/km² even in non-urban parts; residential rangelands never below 10 persons/km²; cultured woodlands usually < 3 persons/km²; rain-fed croplands are the most extensive populated anthrome; by 2000 over half of ice-free land was rangeland, cropland, villages or dense settlements; wildlands without people or land use cover less than a quarter of the terrestrial biosphere. [V] — [Wikipedia: Anthropogenic biome](https://en.wikipedia.org/wiki/Anthropogenic_biome)
- Counting rangelands as anthromes is what takes the anthropogenic share above three quarters of land; without them it is under half. [V-snippet] — [Land Portal summary of Ellis & Ramankutty](https://data.landportal.info/pt/library/browse/by/3843?page=11)

### Inferences
- **Ellis & Ramankutty thresholds [R, not re-verified]**: population classes dense > 100, residential 10–100, populated 1–10, remote < 1 persons/km²; a land use counts as "dominant" from ~20 % cell cover (crops, pasture, irrigated, rice); urban cores > ~2,500 persons/km²; villages = > 100 persons/km² with agriculture; wildlands = no population and no use.
- **Rule set for the game [I]** (inputs: population density `pop` from the history sim, distance to settlement/road/river, suitability scores from §8, the natural biome):
  1. `settlement`: cells of a site's built footprint (pop > ~100/km² pre-industrial towns; village cores smaller). Area ∝ population (pre-industrial towns are dense: ~100–300 persons/ha inside walls).
  2. `cropland`: within the farming radius of a settlement (walking distance ~3–5 km for villages; larger along rivers/roads), highest crop-suitability cells first, until the area needed is met — pre-industrial need ≈ 0.3–1 ha of cropland per person, more with fallow, less with irrigation/rice [R-level estimate; tune]. Needs GDD5 ≥ ~1000–1200 for cereals, α ≥ ~0.4–0.5 or irrigation water (river discharge, oasis), slope < ~8–15° (terraces extend this for dense cultures).
  3. `pasture`: next ring, and everywhere crops fail but grass grows: AI 0.2–0.65 steppe/savanna, alpine meadow (seasonal), heath/moor, cleared forest on poor soil. Nomadic rangeland where pop < ~1–10/km².
  4. `plantation`: tree/shrub crops in a suitable narrow climate near a port, road or river: tropical humid (Tcold ≥ 18, MAP ≥ 1500 — oil palm, cacao, rubber, sugar), Mediterranean (olive, vine — Csa/Csb), subtropical highland (tea, coffee — Cwb/Cfb at 1,000–2,000 m), oasis (date palm). Assign when the owning civilisation's economy supports cash crops; otherwise treat as cropland.
  5. Remaining land keeps its natural biome but with a **disturbance tag** near people (coppice/secondary forest, grazed woodland): this is where Mediterranean scrub, heath and many savannas come from historically — let long-settled regions bias toward the degraded variant.
- Store land use as an **overlay** on the potential natural biome, not a replacement: abandonment (ruins, depopulated sites) then reverts by succession automatically, and the natural biome stays available for flora/fauna tables. [I]

### Gaps
- The full Ellis & Ramankutty 2008 / Ellis 2010 threshold tables and per-anthrome area shares were not retrieved (paper not opened; Wikipedia gives only fragments).
- Per-person cropland need for pre-industrial farming is an estimate without a source.

---

## 10. Transitions, mosaics and natural-looking boundaries

### Takeaway
Real biome boundaries are gradients tens to hundreds of kilometres wide, sharpened locally by feedbacks (fire, treeline) and by terrain. Classify from **continuous suitability scores** and store the top two biomes with a weight; break lines with multi-scale noise on the inputs (not on the output), terrain modifiers (aspect, valley bottoms), and patch disturbance.

### Cited Findings
- The arctic forest–tundra transition averages ~145 km wide in NW Canada. [V] — [Wikipedia: Tree line](https://en.wikipedia.org/wiki/Tree_line)
- Aspect shifts the treeline by hundreds of metres (3,550 m vs 3,250 m on NE slopes in Colorado; 3,200 m west vs 3,600 m east in Yosemite); snow gullies depress it ~400 m. [V] — [Wikipedia: Tree line](https://en.wikipedia.org/wiki/Tree_line)
- Forest–savanna boundaries are sharp and history-dependent (alternative stable states), not smooth functions of rainfall. [V-snippet] — [Staver et al. 2011](https://collaborate.princeton.edu/en/publications/the-global-extent-and-determinants-of-savanna-and-forest-as-alter/)
- TerraFirmaCraft makes placement probabilistic toward the edges of a climate range: density is highest at the centre of a feature's temperature/rainfall range and falls to zero at the edge; forests "place trees closest to the centre of their climate range", and a `use_weirdness` flag trades climate accuracy for variety. [V-snippet] — [TerraFirmaCraft docs: decorators](https://terrafirmacraft.github.io/Documentation/1.18.x/worldgen/decorators); [TFC docs: trees and forests](https://terrafirmacraft.github.io/Documentation/1.18.x/worldgen/features/trees)
- Minecraft picks, for a point in parameter space that fits no biome's intervals, the *nearest* biome interval — a distance-in-climate-space rule that guarantees total coverage. [V] — [Minecraft Wiki: World generation](https://minecraft.wiki/w/World_generation)
- Biome.jl's BIOME4 implementation scores each PFT with a Gaussian-like fitness on standardized distance outside its envelope, instead of a hard cutoff. [V] — [Biome.jl BIOME4 docs](https://clechartre.github.io/Biome.jl/dev/biome4)

### Inferences (all [I])
- **Scores, not boxes**: for each zonal biome compute a fitness in [0,1] as the product (or min) of smooth ramps on each limit (e.g. logistic with width ≈ 1–2 °C on temperature limits, ≈ 10–15 % on moisture limits). Winner = argmax; store `(biome1, biome2, w)` where w is the normalised margin. Region generation then mixes flora/tiles by w, so a 16 m voxel region inside an ecotone gets a mosaic rather than a line.
- **Perturb inputs, not outputs**: add low-amplitude multi-octave noise to the *climate inputs* before classification (±0.5–1.5 °C, ±10–20 % precipitation, at wavelengths of ~5–50 km) plus a soil/substrate noise. Boundaries then wander plausibly and stay consistent across all biomes (dithering the output produces salt-and-pepper). Noise must be a stateless hash of coordinates to stay deterministic.
- **Terrain modifiers at sub-cell scale** (apply when a region is generated at full resolution):
  - *Aspect*: equator-facing slopes get more radiation: ΔT_eff ≈ +1…+2 °C and PET +10–30 % at slopes of 20–30° in mid-latitudes (the reverse on pole-facing slopes); the effect vanishes at the equator and grows with latitude. Result: forest on pole-facing, grass/scrub on equator-facing slopes in semi-arid mountains; higher treeline on sunny slopes.
  - *Riparian corridors*: any cell within the river's water-table influence (distance < ~50–500 m scaled by discharge; height above channel < ~2–5 m) gets its moisture raised to "no water limit" → gallery forest through savanna, cottonwood/tamarisk ribbons and palm oases through desert. The corridor biome = the forest type the *temperature* allows.
  - *Frost hollows / cold-air pools*: closed or flat-bottomed valleys get colder minima → treeless meadows or inverted treelines.
  - *Wind exposure*: ridges and windward coasts → stunted forest, heath, coastal scrub.
  - *Rain shadow at fine scale*: already in the climate sim at coarse scale; a slope-facing-wind bonus on precipitation adds within-cell contrast.
- **Disturbance patches**: stamp patches of early-successional variants with a stateless roll — fire scars (boreal: return interval 50–200 yr, patches km²-scale; Mediterranean 20–50 yr; savanna 1–5 yr), windthrow, floods, landslides on steep wet slopes, avalanche tracks cutting the subalpine forest. Represent as an "age since disturbance" value drawn per patch; young = grass/shrub/birch-aspen stage of the same biome.
- **Minimum patch size and cleanup**: after classification, remove zonal patches smaller than a few cells by majority filter — but never filter azonal biomes (wetlands, oases, lava, riparian), which are legitimately small.
- **Ecotone biomes are real biomes in the list**: temperate woodland-savanna (forest–steppe), semi-desert steppe (steppe–desert), thorn woodland (dry forest–scrub), temperate mixed forest (deciduous–taiga), krummholz. Assign them where the top-two weight is near 0.5 between the right pair, which gives them correct widths automatically.

### Gaps
- No quantitative source for aspect effects on effective temperature/PET or for disturbance return intervals was fetched; the numbers are recalled orders of magnitude.
- No source was found on ecotone widths other than the forest–tundra figure.

---

## 11. Validation targets: Earth's area shares

### Takeaway
Compare a generated world's Köppen histogram against Peel's: B 30.2 %, D 24.6 %, A 19.0 %, C 13.4 %, E 12.8 % of land; drylands (AI < 0.65) 47.2 %. A world that departs far from these has a climate problem upstream of the biome classifier.

### Cited Findings
- Global land by Köppen main class: arid B 30.2 %, cold D 24.6 %, tropical A 19.0 %, temperate C 13.4 %, polar E 12.8 %. Most common types: BWh 14.2 %, Aw 11.5 %. Csc never occurs; Cwc covers 0.002 %. [V] — [Peel et al. 2007](https://hess.copernicus.org/articles/11/1633/2007/hess-11-1633-2007.pdf)
- By continent [V] — [Peel et al. 2007](https://hess.copernicus.org/articles/11/1633/2007/hess-11-1633-2007.pdf):
  - Africa: B 57.2 %, A 31.0 %, C 11.8 %.
  - Asia: D 43.8 %, B 23.9 %, A 16.3 %, C 12.3 %, E 3.8 %.
  - North America: D 54.5 %, B 15.3 %, C 13.4 %, E 11.0 %, A 5.9 %.
  - South America: A 60.1 %, C 24.1 %, B 15.0 %, E 0.8 %.
  - Europe: D 44.4 %, B 36.3 %, C 17.0 %, E 2.3 % (Peel's "Europe" extends east and south-east, which is why B is so large).
  - Australia: B 77.8 %, C 13.9 %, A 8.3 %.
- Aridity classes: hyper-arid 7.5 %, arid 12.1 %, semi-arid 17.7 %, dry sub-humid 9.9 % of land. [V] — [Wikipedia: Aridity index](https://en.wikipedia.org/wiki/Aridity_index)
- Human use: over half of ice-free land used by 2000; wildlands under a quarter. [V] — [Wikipedia: Anthropogenic biome](https://en.wikipedia.org/wiki/Anthropogenic_biome)

### Inferences
- **Approximate biome shares of land [R — recalled, order-of-magnitude only; WWF/Olson 2001 biome areas]**: deserts and xeric shrublands ~19 %; tropical/subtropical grasslands and savannas ~14–15 %; tropical moist broadleaf forest ~13–14 %; boreal forest/taiga ~10–11 %; temperate broadleaf and mixed forest ~9 %; tundra ~8 % (ice-free); temperate grasslands ~7 %; ice and rock ~10 %; montane grasslands ~3.5 %; temperate conifer ~3 %; Mediterranean ~2 %; tropical dry broadleaf ~2 %; flooded grasslands ~0.7 %; tropical conifer ~0.5 %; mangroves ~0.1 %. Use only as a sanity band (± a factor of 1.5).
- **Marine [R]**: ocean 71 % of the surface; continental shelves ~7–8 % of ocean area; mean ocean depth ~3,700 m; abyssal depths (3,000–6,000 m) are the majority of the sea floor; hadal (> 6,000 m) ~1–2 %; coral reefs ~0.1–0.2 % of ocean area (~250,000–300,000 km²); sea ice ~15–25 million km² in total, varying seasonally.
- Wetlands are ~5–8 % of land and peatlands ~3 % [R] — a generated world with 20 % swamp (a common result of a "low drainage = swamp" table) is off by a factor of several.
- The sensible automated check [I]: a `forge_quality`-style gate with wide tolerance bands on (a) Köppen main-class shares, (b) drylands share, (c) forest : grass+shrub : desert : ice ratio, (d) wetland share, (e) sand share of deserts (20–25 %), for worlds configured as Earth-like.

### Gaps
- Per-subtype Köppen shares beyond BWh and Aw were not in the extracted text of Peel (they are in a figure); Beck 2018/2023 give a full table but could not be opened.
- WWF biome area shares and all marine shares are recalled, not sourced this session.

---

## 12. How other generators do it

### Takeaway
Games span a spectrum from pure lookup tables on two noise fields (Azgaar, Minecraft) to climate-driven continuous placement (TerraFirmaCraft). None found does potential-vegetation modelling with seasonality; the two transferable ideas are TFC's "density peaks at the centre of a climate range" and Minecraft's "nearest interval in parameter space".

### Cited Findings
- **Minecraft 1.18+**: six parameters — temperature (5 levels: −1…−0.45, −0.45…−0.15, −0.15…0.2, 0.2…0.55, 0.55…1), humidity (5 levels: −1…−0.35, −0.35…−0.1, −0.1…0.1, 0.1…0.3, 0.3…1), continentalness (mushroom fields −1.2…−1.05, deep ocean …−0.455, ocean …−0.19, coast …−0.11, near-inland …0.03, mid-inland …0.3, far-inland …1), erosion (7 levels), weirdness (peaks-and-valleys PV = 1 − |3|w| − 2|; valleys/low/mid/high/peaks), and depth (cave biomes). Each biome is a set of intervals in this 6-D space; a point outside all intervals takes the closest. Oceans are chosen by continentalness + temperature only; inland biomes by continentalness, erosion and PV, with temperature/humidity picking the variant. Temperature and humidity are pure noise, unrelated to latitude or terrain. [V] — [Minecraft Wiki: World generation](https://minecraft.wiki/w/World_generation)
- **Azgaar's Fantasy Map Generator**: a `biomesMartix` lookup indexed by a moisture band (moisture/5, capped at 4) and a temperature band (20 − temperature, clamped 0–25), with special cases before the lookup: temperature below −5 °C → glacier; wetland if moisture > 40 at low height or > 24 at height 24–60, and temperature above −2 °C; 13 biomes (marine, hot desert, cold desert, savanna, grassland, tropical seasonal forest, temperate deciduous forest, tropical rainforest, temperate rainforest, taiga, tundra, glacier, wetland). [V-snippet of the code; the file itself returned 404] — [Azgaar FMG biomes.js (mirror)](https://git.korz.dev/backups/Fantasy-Map-Generator/src/tag/v1.99/modules/biomes.js)
- **TerraFirmaCraft**: world climate is average annual temperature (typically −25…+30 °C) and rainfall (0–500 mm in TFC units) per position; every feature has a `tfc:climate` placement with optional min/max temperature, min/max rainfall and min/max forest density, optionally "fuzzy" (probability peaks at the range centre, zero at the edges); each forest entry lists min/max rain and temperature per tree species; forests choose the trees closest to the centre of their climate range; forest density is a separate 0–4 field. Biomes in TFC are landforms (terrain shape); vegetation is placed by climate on top. [V-snippet] — [TFC docs: decorators](https://terrafirmacraft.github.io/Documentation/1.18.x/worldgen/decorators); [TFC docs: trees](https://terrafirmacraft.github.io/Documentation/1.18.x/worldgen/features/trees); [TFC docs: common types (1.21)](https://terrafirmacraft.github.io/Documentation/1.21.x/worldgen/common-types)

### Inferences
- **Dwarf Fortress [R; the project already implements this]**: biome = f(rainfall, drainage, temperature, elevation, salinity) with a rainfall × drainage table — desert types by drainage (sand at low drainage, rocky wasteland mid, badlands high), grassland/savanna/shrubland by rainfall, marsh vs swamp by rainfall at low drainage, forest at high rainfall with good drainage; tropical/temperate by latitude-derived temperature; no seasonality in classification. Its weakness for realism is that "drainage" is a noise field rather than a terrain/soil result — the overhaul replaces it with water table + soil.
- **WorldEngine [R]**: implements Holdridge life zones directly from simulated temperature and precipitation/humidity bands (about 40 biomes) — inherits Holdridge's blindness to seasonality.
- **The separable design in TFC is the best match for this project [I]**: landform (terrain) and vegetation (climate) are independent layers, vegetation is continuous in climate space, and species each carry their own climate envelope. For a 72-biome list that means: the biome label is a summary for the map and for gameplay tables, while actual tree/plant placement in a region can use per-species envelopes (Tcold, GDD5, α, soil) — ecotones then come out for free.
- **Minecraft's lesson [I]**: add terrain-derived axes (continentalness, erosion, PV) to the *azonal* tests — the project has real equivalents (distance to coast, slope/relief, flow accumulation) — and guarantee coverage with a nearest-match fallback so no cell is ever "no biome".

### Gaps
- Not researched this session (no time/sources): Undiscovered Worlds, Vintage Story, Songs of Syx, No Man's Sky; Dwarf Fortress and WorldEngine statements are recalled, not fetched. Azgaar's matrix rows and habitability values could not be retrieved (404 on GitHub raw, the blog and the mirror).

---

## 13. Synthesis: recommended classification algorithm

### Takeaway
Classify in passes — (0) derive indices, (1) hard physical states, (2) water and coast, (3) zonal climate vegetation by a plant-limit sieve, (4) azonal substrate/hydrology overrides, (5) fire/disturbance state, (6) land-use overlay — and keep the zonal result underneath every override so that ecotones, succession and flora tables still have it.

### Cited Findings
- The order "B before A/C/D; E by Thot < 10" is from Köppen as specified by Peel. — [Peel et al. 2007](https://hess.copernicus.org/articles/11/1633/2007/hess-11-1633-2007.pdf)
- "Sieve by physiological limits, then a dominance hierarchy" is BIOME1's structure. — [Prentice et al. 1992](https://pure.iiasa.ac.at/id/file/9128)
- Treeline test: GS_len ≥ 94 d and GS_T ≥ 6.4 °C. — [Paulsen & Körner 2014](https://doc.rero.ch/record/324784?ln=fr)
- Bistable band 1000–2500 mm, dry season < 7 months. — [Staver et al. 2011](https://collaborate.princeton.edu/en/publications/the-global-extent-and-determinants-of-savanna-and-forest-as-alter/)
- Dryness classes. — [Wikipedia: Aridity index (UNEP 1992)](https://en.wikipedia.org/wiki/Aridity_index)

### Inferences (the algorithm is my design [I])

**Pass 0 — derive** the indices in the table at the top (PET, AI, α, GDD5/GDD0, Tcold/Thot, DryM, GS, Köppen label, fetch, TWI, sand supply, soil fields). Köppen is stored for validation and UI ("climate: Csa").

**Pass 1 — sea cells** (elevation < sea level): sea ice → trench (> 6,000 m) → abyssal plain (3,000–6,000 m, flat) → open ocean (> 200 m) → shelf sea; then shallow overlays in this order: estuary/lagoon → coral reef → kelp forest → seagrass meadow; vents/seeps as point features. (§6)

**Pass 2 — hard land states**: ice sheet/glacier (ice field) → lava field (young lava) → lake / salt-soda lake / ephemeral lake / river reach type (hydrology) → geothermal pools (points).

**Pass 3 — shore strip** (cells touching the sea or within the tidal frame): mangrove → salt marsh → mudflat → river delta (container) → rocky shore → sandy shore and dunes; coastal scrub/grassland tag on the next cells inland where exposed. (§5, §6)

**Pass 4 — zonal vegetation** for every remaining land cell, by continuous scores (§10), in this decision order:
1. *Cold limit*: treeline test fails → polar desert (MAP < 250 mm or GDD0 < ~100–150) / arctic tundra / alpine tundra-meadow / tropical alpine (páramo AI ≥ 0.65, puna < 0.65) / bare rock and scree (slope > 35–40° or above the vegetation limit).
2. *Dry limit*: AI < 0.05 hyper-arid; AI < 0.2 desert (hot/cold; surface type from Pass 5); AI 0.2–0.35 semi-desert steppe.
3. *Semi-arid to sub-humid* (AI 0.2/0.35–0.65): tropical → thorn scrub (AI < 0.35) / thorn woodland / savanna; winter-rain (Cs) → Mediterranean scrub; temperate → grassland (steppe), shrubland on thin or rocky soil or where winter-rain cold-steppe; AI 0.5–0.75 → temperate woodland-savanna.
4. *Humid* (AI ≥ 0.65): forest type from the leaf-type table (§3); Mediterranean woodland for Cs with AI ≥ 0.5; heath and moor where forest "should" grow but soil is acid/poor and climate cool, wet and windy (Cfb/Cfc coasts and uplands, Thot < ~15–16 °C, high wind exposure, or long grazing/burning history).
5. *Montane specials*: cloud forest (cloud-belt test), tropical conifer forest (Tcold 5–15.5 °C, dry winter, |lat| < ~30°, usually 800–3,000 m or poor soils), temperate rainforest.

**Pass 5 — azonal overrides on land** (substrate and water; the zonal biome is kept as `potential`):
desert surface (salt flat/playa → sand desert → badlands → rocky wasteland), badlands also in semi-arid cells; wetlands in the §5 order (tropical peat swamp forest → peat bog → fen → swamp → marsh → flooded grassland → seasonal floodplain); oasis and springs; riparian corridor (not a listed biome: implement as the gallery-forest variant of the corridor's forest type).

**Pass 6 — dynamic state**: fire/bistability roll for the tropical 1000–2500 mm band and the temperate forest–steppe band (§3); disturbance/succession patches (§10).

**Pass 7 — underground** (per column, independent of the surface biome): groundwater/aquifer, dry caves, underground streams/pools, sea caves from rock type + recharge + position (§7).

**Pass 8 — land-use overlay**: settlement → cropland → plantation → pasture from the history sim (§9).

**Compact rule table for all 72 biomes [I unless a threshold is cited in the section named]**

| # | Biome | Core numeric rule | See |
|---|---|---|---|
| 1 | Ice sheet / glacier | ice-cover field; else Thot ≤ 0 (EF) with snowfall > melt | §2 |
| 2 | Polar desert | Thot < 10, MAP < 250 mm, no ice; GDD0 < ~150 | §2, §4 |
| 3 | Arctic tundra | treeline test fails at sea level; Thot 0–10; GDD0 ≥ ~150 | §2 |
| 4 | Alpine tundra / meadow | treeline test fails only because of elevation; below snow line | §2 |
| 5 | Tropical alpine (páramo, puna) | as 4 with |lat| < 23.5° or annual T range < 5 °C; páramo AI ≥ 0.65, puna < 0.65 | §2 |
| 6 | Bare rock and scree | slope > 35–40°, or between vegetation limit and ice, or fresh deglaciated ground | §2 |
| 7 | Taiga | Tcold ≤ −2 (to −32.5 evergreen; colder → larch), GDD5 350/600–1200, AI ≥ 0.65 (≥ 0.5 on permafrost) | §3 |
| 8 | Temperate conifer forest | Tcold −2…22 with GDD5 ≥ 900 and dry summer (Cs/Ds), or cool continental mountains; AI ≥ 0.65 | §3 |
| 9 | Temperate deciduous broadleaf | Tcold −15(−17.7)…5, GDD5 ≥ 1200, no summer drought, AI ≥ 0.65 | §3 |
| 10 | Temperate mixed forest | both summergreen and boreal/cool conifer pass: Tcold −19…−2, GDD5 900–1500 | §3 |
| 11 | Temperate rainforest | Tcold −2…10, Thot < 22, MAP ≥ 1400 mm, summer share of rain ≥ 10 % (or fog) | §3 |
| 12 | Warm-temperate evergreen (laurel) | Tcold 3/5…15.5, GDD5 ≥ 1200, AI ≥ 0.65 all seasons (Cfa/Cwa/Cfb-subtropical) | §3 |
| 13 | Mediterranean woodland | Cs (Psdry < 40 and < Pwwet/3), Tcold > 0–5, AI 0.5–1.0, soil not thin | §1, §3 |
| 14 | Tropical lowland rainforest | Tcold ≥ 18, Pdry ≥ 60 (or DryM60 ≤ 2), MAP ≥ 2000, below cloud belt (< ~1,000 m) | §1, §3 |
| 15 | Tropical seasonal (monsoon) forest | Tcold ≥ 15.5–18, DryM60 3–5, MAP 1500–2500 (Am; wet Aw fire-protected) | §3 |
| 16 | Tropical dry forest | Tcold ≥ 15.5, DryM60 5–8, MAP 700–1500/2000, fire state = forest | §3 |
| 17 | Cloud forest | tropical/subtropical humid mountain inside cloud belt (≈ 1,200–3,000 m; 500–1,500 m on islands/coastal ranges), windward | §2 |
| 18 | Tropical conifer forest | |lat| < 30°, Tcold 5–15.5 (or poor/sandy soil lowland), dry winter (Cw/Aw), MAP 800–2000 | §3 |
| 19 | Thorn woodland | Tcold ≥ 15.5, MAP 400–800, DryM60 ≥ 8, AI 0.3–0.5 | §3, §4 |
| 20 | Temperate grassland (steppe/prairie) | Tcold < 15.5, AI 0.35–0.65 (BSk and dry Dfa/Dfb/Cfa), deep soil | §4 |
| 21 | Tropical savanna | Tcold ≥ 15.5, MAP 500–2500 with DryM60 ≥ 3; in 1000–2500 only when fire state = savanna | §3 |
| 22 | Temperate woodland-savanna | Tcold < 15.5, AI 0.5–0.75, patch state between forest and steppe | §3, §10 |
| 23 | Shrubland | AI 0.2–0.5 non-tropical with winter rain or thin/rocky soil; also the degraded stage of dry forests | §4 |
| 24 | Mediterranean scrub | Cs, AI 0.2–0.5, or Cs woodland on thin soil / high fire frequency | §1, §3 |
| 25 | Heath and moor | Cfb/Cfc/ET-margin, Thot < 15–16, AI > 1, acid poor soil, wind-exposed; below treeline | §8, §13 |
| 26 | Thorn scrub | Tcold ≥ 15.5, MAP 250–500, AI 0.2–0.35 | §4 |
| 27 | Flooded grassland | flat basin, seasonal inundation 3–8 months, Aw/Cwa climate | §5 |
| 28 | Sand desert | AI < 0.2, sand-supply sink, slope < 3° | §4 |
| 29 | Rocky wasteland (hamada/reg) | AI < 0.2, no sand sink, resistant rock/gravel plain | §4 |
| 30 | Badlands | AI 0.1–0.5, soft sedimentary rock/ash/loess, slope 10–40°, high drainage density | §4 |
| 31 | Semi-desert steppe | AI 0.2–0.35 (lower BS), any temperature | §4 |
| 32 | Cold desert | AI < 0.2, MAT < 18, Tcold < 0 | §4 |
| 33 | Hyper-arid desert | AI < 0.05 | §4 |
| 34 | Salt flat / playa | closed-basin floor, PET > inflow, dry most of the year, slope ≈ 0 | §4 |
| 35 | Lava field | volcanism + young lava (age < 10²–10⁴ yr by climate) | §4 |
| 36 | Marsh | water table ≤ 0.3 m most of the year, standing water 0.2–1.5 m, herbaceous, mineral soil | §5 |
| 37 | Swamp | wetland + treeline passes + shallow/seasonal water, fresh | §5 |
| 38 | Mangrove swamp | intertidal, sheltered, SSTmin ≥ 20 (≥ 16 humid coasts), no hard frost (freeze threshold −6.3…−7.6 °C extreme) | §5 |
| 39 | Salt marsh | intertidal, sheltered, saline, fails mangrove temperature test | §5 |
| 40 | Peat bog | AI ≥ 1.0–1.2, Thot < 17–20, rain-fed, acid; peat ≥ 40 cm | §5 |
| 41 | Fen | permanent groundwater-fed waterlogging, pH > ~5–5.5 (base-rich rock), cool-temperate | §5 |
| 42 | Tropical peat swamp forest | Tcold ≥ 18, MAP ≥ 2000–2500 with DryM60 ≤ 1–2, flat lowland, no mineral sediment | §5 |
| 43 | Seasonal floodplain | within flood reach of a river, inundated < 3 months, alluvium | §5 |
| 44 | River delta | river mouth with discharge above threshold, depositional, low wave/tide energy | §5 |
| 45 | Lake | filled depression with outlet | §7 |
| 46 | Salt / soda lake | terminal lake of closed basin; soda if volcanic catchment | §7 |
| 47 | Seasonal / ephemeral lake | closed basin, AI < 0.2–0.3, dries seasonally or most years | §7 |
| 48 | River (5 types) | upland (slope > 0.5–1 %), lowland (< 0.1 %), seasonal (Qmin/Q < 0.1), episodic arid (flow < 2 months), freeze-thaw (Tcold < −5…−10 for ≥ 3 months) | §7 |
| 49 | Oasis and springs | AI < 0.2 and water table within ~5 m, or exotic-river bank | §7 |
| 50 | Geothermal pools | volcanism/heat flow high + groundwater | §7 |
| 51 | Sandy shore and dunes | exposed/moderate coast, low slope, sand supply | §6 |
| 52 | Rocky shore | coastal slope > 10–15°, resistant rock, long fetch, low sediment | §6 |
| 53 | Mudflat | intertidal, sheltered, fine sediment, large tidal range | §5, §6 |
| 54 | Coastal scrub / grassland | 0.1–5 km from exposed coast, salt spray and wind | §6 |
| 55 | Estuary / lagoon | river mouth with tidal mixing, or barrier-enclosed water, depth < 10–20 m | §6 |
| 56 | Open ocean | depth > 200 m, ice-free | §6 |
| 57 | Sea ice | SSTmin ≤ −1.8 °C | §6 |
| 58 | Shelf sea | depth 0–200 m | §6 |
| 59 | Coral reef | SSTmin ≥ 18, SSTmax ≤ 30–31, depth < 30–50 m, salinity 30–40, clear water away from big river plumes | §6 |
| 60 | Kelp forest | rock, depth < 30–40 m, SSTmax ≤ ~20, nutrient-rich/upwelling | §6 |
| 61 | Seagrass meadow | soft sediment, sheltered, depth < z(11 % surface light) | §6 |
| 62 | Abyssal plain | sea floor 3,000–6,000 m, flat | §6 |
| 63 | Trench | sea floor > 6,000 m at subduction zones | §6 |
| 64 | Hydrothermal vents / cold seeps | ridge/back-arc volcanism (vents); sediment-rich margins and subduction wedges (seeps) | §6 |
| 65 | Dry caves | soluble rock or lava tubes above the water table | §7 |
| 66 | Underground streams / pools | carbonate rock + MAP > 500–600 mm + relief | §7 |
| 67 | Groundwater / aquifer | porous/fractured rock + recharge + confining layer | §7 |
| 68 | Sea caves | rocky exposed coast at sea level, jointed/soluble rock | §7 |
| 69 | Cropland | in farming radius, GDD5 ≥ 1000–1200, α ≥ 0.4–0.5 or irrigation, slope < 8–15°, best soil first | §9 |
| 70 | Pasture | used land failing the crop test; AI ≥ 0.2; pop ≥ ~1/km² | §9 |
| 71 | Plantation | cash-crop climate niche near transport, economy flag | §9 |
| 72 | Settlement | site footprint from the history sim (pop > ~100/km²) | §9 |

**Fields needed beyond the planned list [I]**: prevailing wind direction (sand transport, windward/leeward, wave fetch weighting); relative humidity or cloud-base height (cloud forest, fog desert); tidal range or a proxy (mudflat, salt marsh, mangrove widths); lava / surface age (lava field, soil depth); a carbonate/evaporite and porosity distinction inside rock type (karst, aquifers); a turbidity proxy from river sediment load (reefs, seagrass); soil pH/base status derived from rock (bog vs fen, heath). Monthly snow cover/depth would improve tundra, treeline gullies and freeze-thaw rivers but can be derived from monthly T and P.

### Gaps
- Every threshold marked [I] is a starting value to be tuned against the validation shares in §11 and by looking at maps; none is a literature constant.
- The heath-and-moor, shrubland and woodland-savanna rules are the least well grounded: on Earth all three are strongly shaped by land-use history and fire, not by climate alone.
- IUCN Global Ecosystem Typology (Keith et al. 2022), the single most relevant source for per-biome ecological drivers (it covers nearly every one of the 72 as a functional group), was not opened this session; it should be the first reference checked when these rules are turned into a spec.
