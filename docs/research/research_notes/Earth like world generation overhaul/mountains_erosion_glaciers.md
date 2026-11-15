# Mountains by age and type, erosion processes, glaciers and glacial landforms (offline world generator)

Reading guide. "Cited Findings" are statements I read in the source (paper text, documentation page) or in a
search-result summary of it; the latter are marked *(search summary)*. "Inferences" are my own derivations,
recipes and arithmetic, plus a few things recalled from memory that I could not re-verify in this session —
those are marked *(recalled, unverified)*. Four of the requested papers could not be opened in full text
(Argudo 2020, Schott 2023, Tzathas 2024, Galin 2019 — bot-blocked or 404); for these only abstract-level
facts are cited. Cordonnier 2016, 2017, 2018 and 2023 were read from the PDFs.

## 1. Mountain age: how relief evolves after uplift stops, and how a generator can simulate it

### Takeaway
Plain detachment-limited stream power (what the generator has now) flattens a dead range in only 1–10 Myr, so
"old but still standing" mountains (Appalachians, Urals) need extra physics: isostatic rebound (up to ×6
longer), sediment cover / transport-limited behaviour (×2–3) and an erosion threshold (×~20). Age therefore
has to be modelled as a per-belt uplift history followed by an erosion-only phase with rebound, not as a
filter on a finished heightmap.

### Cited Findings
- The standard detachment-limited stream power incision model predicts post-orogenic decay times of only
  1–10 Myr for reasonable initial topographies; pure Airy isostatic rebound of a thick crustal root lengthens
  this by at most a factor of 6; a switch to transport-limited conditions (thin alluvial cover shielding the
  bed) adds a factor of 2–3; a critical shear stress for erosion adds about a factor of 20 in the authors'
  example; combined, these can explain residual topography lasting hundreds of Myr. A significant share of the
  topography remains once predicted lowering rates fall below measured denudation rates of low-relief,
  weathering-limited landscapes. — [Baldwin, Whipple & Tucker 2003, JGR 108(B3), abstract](https://umimpact.umt.edu/en/publications/implications-of-the-shear-stress-river-incision-model-for-the-tim/)
- Orogenic decay is set by erosional efficiency and has two phases with different isostatic rebound behaviour
  and time scales; in coupled models a thick, strong lithosphere keeps a maximum topography of about 1500 m
  even after 150 Myr of decay, while weaker cases lose all orogenic topography within a few tens of Myr. The
  same paper classes the Southern Alps (NZ) as an orogen where surface processes dominate, and the Urals and
  Appalachians as inactive orogens whose topography decays slowly over tens to several hundreds of Myr.
  *(search summary)* — [GFZ Potsdam paper on mountain-belt topography, rheology and surface processes](https://gfzpublic.gfz-potsdam.de/pubman/item/item_5011946_3/component/file_5012394/5011946.pdf)
- Erosion unloads the lithosphere and the rebound offsets part of the relief loss (worked example: Dabie
  Shan). *(search summary)* — [Braun & Robert 2005, ESPL](https://www.isterre.fr/IMG/pdf/Braun-and-Robert-ESPL2005.pdf)
- Old ranges are not simply "decayed": in the southern Appalachians (Cullasaja basin) relief has increased by
  more than 150% since the Miocene, attributed to post-orogenic regional uplift (rejuvenation). *(search
  summary)* — [GSA Today 23(2)](https://rock.geosociety.org/net/gsatoday/archive/23/2/pdf/i1052-5173-23-2-4.pdf)
- Measured erosion rates span two orders of magnitude between old and young ranges: Great Smoky Mountains
  25–30 m/Myr, spatially homogeneous ([USGS record of Matmon et al.](https://pubs.usgs.gov/publication/70024865));
  Blue Ridge escarpment basins 5.4–49 m/Myr, rising with mean basin slope
  ([AJS article](https://ajsonline.org/article/61707.pdf)); Hangay Mountains (Mongolia) 12–20 m/Myr
  ([Georgia Tech repository](https://repository.gatech.edu/server/api/core/bitstreams/2d9c225c-4ae8-4eaa-ad5f-948b9cfe3a43/content));
  High Himalaya 2.7 ± 0.3 mm/yr (= 2700 m/Myr), 1.2 mm/yr near the Tibetan margin, below 0.6 mm/yr in the
  foothills, with a log-linear relief–erosion-rate relation over three orders of magnitude
  ([Vance et al. 2003, EPSL](https://ui.adsabs.harvard.edu/abs/2003E%26PSL.206..273V/abstract)). *(all search summaries)*
- Whipple (2001) derives analytical response times of detachment-limited channels to tectonic/climatic
  change (Taiwan Central Range case); they are minimum estimates; steady state is plausible under stable
  climate but Quaternary climate swings probably prevent it in modern orogens. *(search summary; the numeric
  response times were not retrieved)* — [Whipple 2001, AJS 301, listing](https://data.zhenzhi.com/details/1/893730763)
- Cordonnier et al. 2016 calibration of the same law the generator uses (m = 0.5, n = 1): maximum uplift
  U = 5.0·10⁻⁴ m/yr ("the average uplift among earth mountains"), erosion constant k = 5.61·10⁻⁷ yr⁻¹ so
  peaks culminate near 2000 m, time step δt = 2.5·10⁵ yr, convergence in 100–300 steps; only the ratio u/k
  matters and peak height is linear in it: h_max = 2.244·u/k (their fit; with the values above this gives
  2000 m). Erosion without uplift "converges toward a flat terrain", so they use small steps and stop after a
  few iterations. — [Cordonnier et al. 2016, CGF 35(2), PDF](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonier16CGF.pdf)
- Cordonnier et al. 2018: isostasy turns crustal thickening into surface uplift; the uplift map is derived
  from a viscous thin-sheet crust compressed by moving plates on a 1024² grid, then refined by procedural
  multi-scale folding of rock layers; a 5 Myr simulation takes 5 coarse iterations of ~10 ms; the offline
  refinement runs erosion at 1,000,000 vertices, 4 s per step, 100 iterations. — [Cordonnier et al. 2018, IEEE TVCG 24(5), PDF](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier18TVCG.pdf)
- Tzathas et al. 2024 use analytical solutions of the stream power law (previously 1D only) extended to 2D
  heightmaps, solved with a multigrid-accelerated iteration, with landslides and hillslope processes added;
  terrain "at a given time" is produced without stepping a simulation. *(abstract / search summary; full
  text not opened)* — [Tzathas, Gailleton, Steer & Cordonnier 2024, CGF 43(2)](https://diglib.eg.org/handle/10.1111/cgf15033)
- The Braun–Willett scheme is implicit and O(n); it reaches the correct steady state for any step length,
  but the accuracy of *transient* solutions depends on the step. — [Landlab FastscapeEroder docs](https://landlab.readthedocs.io/en/latest/generated/api/landlab.components.stream_power.fastscape_stream_power.html)

### Inferences
- **Why n = 1 alone cannot make old mountains.** With U = 0 and n = 1 the law dh/dt = −K·A^m·S is a pure
  upstream-travelling wave with celerity K·A^m; a point is untouched until the base-level signal arrives
  after τ(x) = ∫ dx′ / (K·A(x′)^m) and is graded to base level afterwards. Relief is gone after max τ, which
  is the 1–10 Myr of Baldwin et al. (the τ integral is the standard Whipple–Tucker response time; *recalled,
  unverified here*). The decay is a retreating front (a dissected plateau edge), not a uniform lowering —
  useful: partly decayed n = 1 landscapes look like escarpments and dissected plateaus on their own.
- **Cheap isostasy = the factor of 6.** Airy compensation returns ρ_c/ρ_m ≈ 2.7/3.3 ≈ 0.82 of every eroded
  metre as rock uplift, so net lowering is ~18% of erosion, i.e. ×5.5 — matching Baldwin's "at most a factor
  of 6" (my arithmetic). Implementation: after each erosion step add U_iso = 0.8 · (low-pass filtered eroded
  thickness). The low-pass (Gaussian of ~50–150 km radius, or a proper thin-plate flexure solve on the coarse
  grid) stands in for flexural rigidity; it also makes valleys cut while summits rise, which keeps peaks high
  in old glaciated ranges. One blur per coarse step: negligible cost.
- **Per-belt age recipe.** Give each belt a record {type, t_start, t_end, U_peak(x), width, vergence}. Run
  the world clock from the oldest orogeny to the present in epochs; uplift at time t is
  Σ_belts U_b(x)·w_b(t) with w ramping up, plateauing and falling to zero at t_end; after t_end only U_iso
  acts. Since transient accuracy needs bounded steps, use ~10⁵ yr steps while a belt is active or recently
  dead and ~10⁶ yr steps for the long quiet tail on the coarse grid. A 300 Myr history is then a few hundred
  coarse steps; only the last few Myr need the fine grid (existing multigrid schedule).
- **Levers that make "old" look right without waiting 300 Myr of simulated time:**
  (a) an incision threshold (erosion = max(0, K·√Q·S − ε_c)) — the largest single factor in Baldwin et al.;
  (b) sediment cover / deposition (Yuan 2019 G term, see section 7) so valleys alluviate and slow down;
  (c) rainfall recomputed from relief (already present) — lower range, less orographic rain, slower decay;
  (d) stratigraphy: as erosion exhumes the hard crystalline core, K drops; ridges in old fold belts follow
  resistant beds (Appalachian Valley-and-Ridge), which the existing per-rock erodibility gives for free if the
  belt's layers are folded;
  (e) hillslope diffusion acting for a long time relative to incision rounds the ridges; scale diffusivity
  with soil cover/climate.
- **Rejuvenation pulse.** Old ranges with sharp gorges (southern Appalachians, Scandinavia, Great Dividing
  Range) are best made as: decay to low relief, then a late broad, low-amplitude uplift (a few hundred metres
  over 10–30 Myr, wavelength several hundred km) → incised plateau with a summit accordance. This is cheap
  and gives the "dissected plateau" look the owner asked for.
- **Quantitative targets by age class** *(recalled general knowledge, unverified in this session; use as
  starting targets, not as data)*: young collisional/active (Himalaya, Alps, Andes, Southern Alps): peaks
  3.7–8.8 km, erosion 1–10 mm/yr, hillslopes at the threshold angle (~30–35° modal), local relief 1.5–3+ km
  per ~5 km window, knife-edge ridges, glacial overprint; middle-aged (Rockies, Pyrenees): peaks 3.4–4.4 km,
  erosion ~0.05–0.5 mm/yr, mixed threshold slopes and soil-mantled slopes; old (Appalachians, Urals, Scottish
  Highlands, Scandes, Great Dividing Range): peaks 1.3–2.5 km, erosion 5–50 m/Myr (cited above for the
  Appalachians), convex soil-mantled ridges, modal slopes 10–20°, local relief 300–800 m, accordant summits.
  The Scandes and Scottish Highlands owe their sharp parts to glaciation, not to youth.
- A usable validation set for the generator: (1) erosion rate map (should span ~10 m/Myr to ~3 mm/yr),
  (2) slope histogram per belt (mode near threshold angle for young, far below it for old), (3) hypsometric
  curve, (4) local relief in a 5 km window.

### Gaps
- No primary source was opened that tabulates local relief, slope distribution, drainage density or ridge
  curvature side by side for young vs old ranges; the class targets above are from memory.
- Whipple's (2001) numeric response times and the Whipple–Tucker τ formula were not re-read.
- Flexural-isostasy parameters (effective elastic thickness, FastScape's flexure component) were not
  retrieved; the FastScape model page lists no flexure process ([fastscape models](https://fastscape.readthedocs.io/en/latest/models.html)).
- Tzathas 2024's transient analytical solution (which would give "terrain at age t" directly) could not be
  read; worth opening manually, it is the closest published match to "age as a parameter".

## 2. Mountain types beyond fold belts: cheap procedural recipes

### Takeaway
The published graphics pipeline is "author an uplift field (and a rock column), let stream power carve it";
every mountain type can be expressed as a different uplift-history shape plus a different rock column, with a
few constructional types (volcanoes, dunes) added as deposited material rather than uplift. I found no paper
giving recipes per type; the recipes below are my proposals.

### Cited Findings
- Uplift maps as the authoring primitive: a painted uplift map plus stream power yields dendritic networks,
  watersheds and ridges; canyons were obtained from "two strokes and a gradient" with a height-dependent
  maximum slope giving alternating cliffs and slopes; a procedurally varied thermal slope (low below a given
  height, high above) adds cliffs to crests. — [Cordonnier et al. 2016](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonier16CGF.pdf)
- Collisional belts: subduction of one plate gives large-scale asymmetry; a main crustal-scale fault forms
  above the non-subducting plate; fold wavelength depends on layer thickness and competence contrast; layered
  uplift lets folded strata show on eroded cliffs. — [Cordonnier et al. 2018](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier18TVCG.pdf)
- Schott et al. 2023 model terrain as uplift rather than elevation, with copy-paste, warping for folds and
  faults, point and curve elevation constraints, and inversion of uplift from an existing DEM; a fast
  approximation of drainage area and flow routing makes it interactive; code is public. *(abstract level)* —
  [project page](https://h-schott.github.io/publications/uplift/publi_uplift.html), [code](https://github.com/H-Schott/StreamPowerErosion)
- Sandstone landforms (arches, alcoves, hoodoos, buttes) have a dedicated 3D simulator combining stress,
  wind and fluvial erosion and particle deposition (Arenite, SIGGRAPH 2025) — a 2025 follow-up from the same
  group; it is a 3D local-scale method, not a heightfield one. — [Yang et al. 2025, ACM TOG 44(4)](https://cs.purdue.edu/homes/bbenes/papers/Yang25ToG.pdf)

### Inferences (recipes; mine, not from a source)
- **Fold-and-thrust belt (collision):** asymmetric uplift profile across strike (steep on the foreland side),
  highest U near the main fault; folded layer stack (sinusoidal layer offsets with wavelength ∝ layer
  thickness) so hard beds make strike ridges; flexural foreland basin = negative uplift band beside the belt.
- **Volcanic arc:** a line of vents 20–80 km apart, parallel to the trench and ~100–300 km inland; each
  stratovolcano is *constructed* (not uplifted): add a radially symmetric cone with concave-up flanks
  (slope ~30–35° near the summit relaxing to ~5° at the base; e.g. h(r) = H·exp(−r/r₀) clipped at a crater),
  made of weak rock (high K) with lava-flow lobes (hard) so erosion cuts radial gullies (parasol drainage)
  and leaves planèzes. Age: young cones are smooth; run erosion on old ones to get dissected cones, necks and
  radial dykes (hard plugs in a soft cone). Calderas: subtract a flat-floored disc and fill with a lake.
- **Shield volcano / hotspot chain:** very wide low cones (flank slope 3–6°) of hard basalt along a line with
  monotonically increasing age; apply erosion time and thermal subsidence proportional to age so the chain
  runs from a tall young shield, through deeply gullied islands, to low remnants/atolls.
- **Fault-block ranges and rifts (Basin and Range, East African Rift):** piecewise-linear tilted-block
  uplift: sawtooth across strike with wavelength 20–40 km, sharp fault scarp on one side, gentle back-slope
  on the other; basins get negative uplift and fill with sediment (needs the deposition term) → bajadas and
  playas. A rift is two inward-facing sawteeth with a central graben, flank uplift decaying over ~100 km, and
  volcanoes on the axis.
- **Plateaus (Tibet, Colorado):** broad uniform uplift of a flat-lying layer stack with a hard cap-rock; low
  rainfall on top. Edge dissection by n = 1 stream power naturally gives retreating escarpments; canyons need
  a trunk river entering from outside (antecedent drainage) and a strong K contrast between layers so walls
  step (cliff = hard layer, slope = soft layer). Mesas and buttes are the outliers left by scarp retreat —
  they appear on their own if the cap-rock is thin and hard and thermal erosion uses a per-rock repose angle.
- **Dome mountains (Black Hills, Adirondacks):** Gaussian uplift bump 50–200 km across under a layer stack;
  erosion exposes concentric rings: hard core in the centre, hogback ridges of tilted hard layers around it,
  a ring valley in soft layers (the layer geometry is the uplift integral, so the existing stratigraphic
  column gives this without extra code).
- **Karst:** in carbonate rock set surface K low but add a dissolution sink: runoff infiltrates (drainage
  area does not accumulate on limestone above the water table), so valleys go dry; stamp closed depressions
  (dolines) with a Poisson-disc distribution, density rising with rainfall; in humid tropical zones invert it
  — stamp residual towers/cones (tower/cockpit karst). Caves are a 3D feature for the voxel stage, not the
  heightmap stage.

### Gaps
- No source was found (or searched in depth) for quantitative volcano profile laws, fault-block geometry
  statistics or karst density laws; the numbers in the recipes are from memory and must be treated as
  starting values.
- Galin et al. 2019, "A Review of Digital Terrain Modeling" (CGF 38(2)) is the natural survey for the
  procedural-primitive side (volcano, mesa and karst generators); it could not be opened in this session.

## 3. Key papers and what each contributes

### Takeaway
The lineage is Braun & Willett's O(n) implicit stream power → Cordonnier 2016 (uplift map + graph) → 2018
(plates, layers, folds) → Schott 2023 (interactive authoring by uplift) → Tzathas 2024 (analytical, no time
stepping), with Cordonnier 2023 adding glacial erosion and Argudo 2020 adding glacier ice as a renderable
object. For a CPU generator the most useful glacial reference is not in graphics at all: Hergarten 2021 /
Deal & Prasicek's glacial stream power laws, which reuse the solver the generator already has.

### Cited Findings
- **Braun & Willett 2013 (FastScape):** implicit, O(n) solution of the stream power law; later extensions
  are also implicit and O(n), allow very large steps (10⁴–10⁶ yr), and cover sediment transport/deposition,
  variable discharge, local minima (sinks) and lake deposition. *(search summaries)* —
  [GFZ FastScape project page](https://gfz.de/en/section/earth-surface-process-modelling/projects/current-projects/fastscape-landscape-evolution-model-development), [EGU 2018 abstract](https://meetingorganizer.copernicus.org/EGU2018/EGU2018-17723-1.pdf?pdf)
- **FastScape (xarray-simlab) model set:** `basic_model` (block uplift, stream power, linear hillslope
  diffusion, D8), `sediment_model` (separate bedrock/soil layers, deposition coefficients `g_coef_bedrock` /
  `g_coef_soil`, multiple-flow-direction routing), `marine_model` (submarine transport of sand and silt by
  diffusion, compaction with depth-decaying porosity, sea level, stratigraphic horizons). No glacial model is
  listed. — [FastScape models documentation](https://fastscape.readthedocs.io/en/latest/models.html)
- **Yuan, Braun, Guerit, Rouby & Cordonnier 2019 (JGR-ES 124, 1346–1365):** an O(N), implicit,
  unconditionally stable method for the Davy & Lague (2009) erosion–deposition form of the stream power law,
  where local balance depends on the sediment flux from upstream; shows autogenic aggradation/incision cycles
  in the foreland at dynamic steady state. *(abstract)* — [ADS record](https://ui.adsabs.harvard.edu/abs/2019JGRF..124.1346Y)
- **Guérit et al. 2019 (Geology):** the dimensionless deposition coefficient G can be estimated from river
  slopes at the catchment–fan transition (29 experimental and 68 natural systems); G = 0 is detachment-limited,
  the studied natural examples give G > 0.4. *(search summary)* — [paper PDF mirror](https://typeset.io/pdf/fluvial-landscape-evolution-controlled-by-the-sediment-1hefaht0hi.pdf)
- **Cordonnier et al. 2016 (CGF):** stream power on a random planar graph (Delaunay/Voronoi cells) instead
  of a grid; lake/depression handling by a tree of lakes connected through lowest passes, cutting the number
  of iterations 4–5×; thermal-erosion slope clamp (30° default); conversion of the stream graph to a
  procedural primitive tree for detail. Timings on a 3 GHz CPU: 10,000 nodes 0.031 s/step (6.4 s to converge),
  40,000 nodes 0.177 s (35.4 s), 90,000 nodes 0.401 s (78 s), 160,000 nodes 1.273 s (252 s); ~200 steps to
  converge; domain 50 × 50 km. — [PDF](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonier16CGF.pdf)
- **Cordonnier et al. 2017 (SIGGRAPH), ecosystem + erosion:** a layered scene (bedrock B, granular layers
  rock R, sand, humus; vegetation layers grass, shrubs, trees) changed by stochastic *events* (runoff,
  rock-/sand-/humus-slides each with its own friction angle, lightning, fire, temperature fracturing of
  bedrock into rocks at a rate reduced by granular and vegetation cover); events move bounded volumes and
  replace a global time-stepped PDE; time scale is years to centuries (e.g. one runoff event per cell per year
  for 200 years). — [PDF](https://cs.purdue.edu/homes/bbenes/papers/Cordonnier17ToG.pdf)
- **Cordonnier et al. 2018 (TVCG):** see section 1 — plates, viscous crust, layered folding, volumetric
  uplift. — [PDF](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier18TVCG.pdf)
- **Schott et al. 2023 (ACM TOG 42(5), art. 162; published title "Large-scale Terrain Authoring through
  Interactive Erosion Simulation", not "…Landscape Evolution"):** see section 2. — [project page](https://h-schott.github.io/publications/uplift/publi_uplift.html)
- **Cordonnier et al. 2023 (SIGGRAPH, ACM TOG 42(4), art. 61) "Forming Terrains by Glacial Erosion":** first
  graphics method coupling glacier dynamics with erosion over glacial cycles; details in sections 4–6. —
  [PDF](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- **Argudo et al. 2020 (SIGGRAPH Asia, ACM TOG 39(6)):** Shallow Ice Approximation simulation on the GPU
  (compute shader) with interactive feedback, plus procedural amplification: crevasses, moraines, seracs,
  ogives and icefalls are synthesised by rules, not simulated. Code is MIT-licensed; inputs are a bedrock
  DEM, optional solar radiance and precipitation maps and an ELA; the starting point was Jarosch's
  mass-conserving SIA scheme ("Restoring mass conservation to shallow ice flow models over complex
  terrain"). *(abstract + repository README)* — [HAL record](https://hal.archives-ouvertes.fr/hal-02929531), [code](https://github.com/oargudo/glaciers)
- **Tzathas et al. 2024 (CGF 43(2)):** see section 1. — [EG digital library](https://diglib.eg.org/handle/10.1111/cgf15033)
- **Hergarten 2021 (ESurf 9, 937–952):** a glacial erosion law of stream-power form implemented in OpenLEM,
  fully implicit with cost linear in grid size; details in section 5. — [paper](https://esurf.copernicus.org/articles/9/937/2021/)
- **Deal & Prasicek (GRL, online 2020-12, DOI 10.1029/2020GL089263) "The Sliding Ice Incision Model":**
  glacial erosion written in terms of catchment area, ice accumulation rate and ice surface slope, analogous
  to stream power, intended to make glacial landscape models as efficient as fluvial ones. *(search summary)*
  — [EGU 2020 abstract](https://meetingorganizer.copernicus.org/EGU2020/EGU2020-5510.html)
- **2025 follow-up:** Arenite (sandstone simulator), see section 2. — [PDF](https://cs.purdue.edu/homes/bbenes/papers/Yang25ToG.pdf)

### Inferences
- The generator already implements the Cordonnier 2016 core (implicit n = 1 along a drainage tree,
  depression filling, multigrid). The cheapest high-value additions, in order: Yuan 2019 deposition term
  (same solver structure), a slope-threshold pass, isostatic rebound, a glacial stream-power term (Hergarten).
- Cordonnier 2017's event formalism suits the *fine* cells (16–32 m) and short time scales (talus, soil,
  vegetation) — a finishing pass, not the geological-time driver.

### Gaps
- Full texts of Argudo 2020, Schott 2023, Tzathas 2024 and Galin 2019 were not readable (HAL/EG bot
  protection, 404 on the LIRIS copy), so their equations, parameter tables and timings are missing here.
- I found no 2025–2026 graphics paper on glacial terrain beyond Cordonnier 2023 in one search; that is weak
  evidence of absence. A benchmark of Hergarten's law against the process-based iSOSIA model was mentioned in
  search output ("cannot replace process-based models but is complementary, mainly for efficiency") but I
  could not pin its URL — unverified.

## 4. Where ice exists: ELA, mass balance, ice-flow models, parameters and stable steps

### Takeaway
Mass balance can be as simple as M = min(p_max, β·(s − E)) with one ELA field E(x) derived from temperature
and precipitation; the expensive part is ice flow. Explicit SIA at 32 m cells needs sub-day steps and, per
Cordonnier 2023, carves wrong (V-shaped) valleys when coupled to erosion — so for a CPU generator use
steady-state ice-flux routing for valley glaciers and reserve SIA for a coarse ice-sheet grid.

### Cited Findings
- Thickness evolution: ∂h/∂t + ∇·(ū h) = M, with h clamped at 0; mass balance M = min(p_max, β (s − E)),
  s = z + h the ice surface, E the equilibrium line altitude; defaults β = 1.0·10⁻² yr⁻¹,
  p_max = 2 m/yr, E = 500–2000 m; ice density 917 kg/m³; Blatter-model rheology constant
  B = 2.9·10⁵ Pa·yr^(1/3) (Glen exponent 3 ⇒ p = 4/3 in the energy form); Weertman-type sliding with exponent
  σ = 3/2 and coefficient C from 500 (fast-flowing) through 1500 (high friction) Pa·m^(−σ)·yr^σ to ∞ (frozen
  bed). — [Cordonnier et al. 2023, Eq. 1–2 and Table 1](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- "The area above [E] is fully covered in ice"; E depends on mean sea-level temperature, which varies by up
  to 10 °C between glacial and interglacial periods in some places; β/precipitation sets how much ice
  accumulates and how fast the glacier flows. — [Cordonnier et al. 2023, §7.4](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- Monthly temperature-index (degree-day) mass balance used by OGGM:
  B_i(z) = P_i^solid(z) − d_f · max(T_i(z) − T_melt, 0); defaults: lapse rate −6.5 K/km, T_melt = −1 °C
  (monthly mean), precipitation all solid below 0 °C and all liquid above 2 °C with a linear blend, no
  precipitation gradient by default; the melt factor d_f and precipitation factor are calibrated per glacier
  (against Hugonnet et al. 2021 geodetic mass balance). — [OGGM mass-balance docs](https://docs.oggm.org/en/stable/mass-balance-monthly.html)
- Degree-day factors: at the ELA of 66 glaciers 3.5 ± 1.4 and 4.6 ± 1.4 mm d⁻¹ K⁻¹ (low/high accumulation
  estimates), close to literature snow values ([Braithwaite et al., Annals of Glaciology](https://resolve.cambridge.org/core/services/aop-cambridge-core/content/view/39D06FC916EA55B616BCEC80832D65CB/S0260305500262162a.pdf/accumulation-at-the-equilibrium-line-altitude-of-glaciers-inferred-from-a-degree-day-model-and-tested-against-field-observations.pdf));
  typical snow values 2–6 mm d⁻¹ °C⁻¹ within a reported 0.5–20 range ([HESS 30, 2613, 2026](https://hess.copernicus.org/articles/30/2613/2026/));
  Greenland ice 5.8–12 mm d⁻¹ °C⁻¹ below 1000 m a.s.l., up to 20 higher up ([J. Glaciol. 2018](https://www.cambridge.org/core/product/B043A6CE2D469695FB1795FA16998715/core-reader)).
  Factors are lower for snow than for ice and rise with elevation, direct solar input and lower albedo (after
  Hock 2003). *(all search summaries)*
- SIA velocity in OGGM's notation: u = u_d + u_s with u_d = f_d·h·τⁿ, u_s = f_s·τⁿ/h, f_d = 2A/(n+2), n = 3,
  basal shear stress taken equal to the driving stress τ = α·ρ·g·h (α = surface slope); the semi-implicit
  scheme is ~1.5× slower per step than the explicit flux scheme but allows ~3× larger Δt; a
  MUSCL-SuperBee scheme exists for mass conservation on very steep walls. — [OGGM ice-dynamics docs](https://docs.oggm.org/en/stable/ice-dynamics.html)
- Glen–Nye law ε̇ = A τⁿ, n usually 3 (plausible 2–4), A in Pa⁻ⁿ s⁻¹, Arrhenius in temperature, estimates
  varying by orders of magnitude ([Wikipedia, citing Cuffey & Paterson 2010](https://en.wikipedia.org/wiki/Glen%E2%80%93Nye_flow_law));
  typical temperate-glacier value A ≈ 75 MPa⁻³ a⁻¹ with n = 3 ([EGUsphere 2024-1600 preprint, Argentière](https://egusphere.copernicus.org/preprints/2024/egusphere-2024-1600/egusphere-2024-1600.pdf), *search summary*).
- Explicit SIA stability (PISM): Δt ≤ 2R / [D (1/Δx² + 1/Δy²)], D = maximum SIA diffusivity, R an adaptive
  ratio < 1; PISM caps D (`max_diffusivity` 100 m²/s) and by default smooths the bed over a 5 km half-width
  to reduce diffusivity over rough beds. — [PISM manual, SIA](https://pism.io/docs/manual/modeling-choices/dynamics/sia.html)
- Explicit advection form: Δt < c·Δx/‖ū‖ with c = 0.1 needed for stability; at Δx = 100 m and ~100 m/yr
  that is ~0.1 yr; glaciology normally needs steps of "a few days". SIA is appropriate where vertical
  shearing dominates (ice caps), "not on mountain glaciers"; SSA is the opposite; the Blatter higher-order
  model fits both. One SIA step costs 4.7 ms at 1024² on their GPU but needs ~100× more steps at 32 m cells.
  — [Cordonnier et al. 2023, §3.1, §5, §7.3](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- Glaciers are close to equilibrium at the time scale of climate change: after an ELA drop from 1600 m to
  900 m over 70 kyr the ice volume barely changes during 30 kyr of constant climate; this is what lets their
  scheme take 10–100 yr steps (error < 1% at 10–20 yr, 10.5% mass loss at 100 yr). — [Cordonnier et al. 2023, §7.2](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- Steady-state alternative with no ice dynamics at all: route ice like water. Per cell, the fraction of
  precipitation turned to ice rises linearly from 0 at the ELA (H_e) to 1 at H_f and is negative (melt) below
  the ELA; ice flux A_i accumulates down the steepest-descent tree; glacier width w ∝ q_i^0.3; thickness from
  a constant thickness-to-width ratio (≈ 0.25 in physical units) or h ∝ q_i^((1−α)/ψ)·S⁻¹; the ice surface
  (bed + h), depression-filled, replaces the bed for slope. Example ELA/H_f: ≈ 2000 m / 2500 m. — [Hergarten 2021](https://esurf.copernicus.org/articles/9/937/2021/)

### Inferences
- **ELA field for the generator (mine):** the generator has temperature and precipitation; compute the
  annual degree-day sum at sea level from mean annual temperature and seasonal amplitude, lapse it with
  6.5 K/km, and define E(x) as the altitude where DDF·PDD(z) = P_solid(z). With DDF ≈ 4 mm d⁻¹ K⁻¹ and
  1 m/yr w.e. of snow, balance needs ~250 positive degree-days, i.e. a summer mean a few degrees above zero —
  so wetter coasts get a lower ELA (maritime glaciers) and dry interiors a higher one, as on Earth. Polar ice
  sheets fall out when E drops to the land surface.
- **SIA numbers (my arithmetic, from f_d = 2A/(n+2) and A ≈ 2.4·10⁻²⁴ Pa⁻³ s⁻¹ = 75 MPa⁻³ a⁻¹):**
  diffusivity D = (2A/5)(ρg)³ h⁵ |∇s|². Valley glacier h = 300 m, slope 0.1: D ≈ 0.017 m²/s ≈ 5.4·10⁵ m²/yr
  → Δt ≤ Δx²/(4D): 32 m cells ≈ 4 hours, 200 m cells ≈ 7 days. Ice sheet h = 2000 m, slope 0.005:
  D ≈ 0.56 m²/s → 10 km cells ≈ 1.4 yr. So explicit SIA is fine for a continental ice sheet on a coarse grid
  (10⁴–10⁵ steps of a small grid for a 100 kyr cycle, or far fewer when relaxing to steady state) and
  hopeless for valley glaciers at final resolution.
- **Recommended split:** (1) ice sheets: SIA to steady state on the planetary coarse grid (5–20 km cells),
  or cheaper, a perfectly-plastic profile h ≈ sqrt(2 τ₀ d / (ρ g)) with τ₀ ≈ 50–100 kPa and d the distance
  to the margin *(recalled Nye profile, unverified)*; (2) valley glaciers: Hergarten-style flux routing on
  the fine grid, O(n) per evaluation, re-evaluated per erosion step; (3) the visible present-day ice body is
  the routed thickness h, smoothed, placed on the final bedrock.

### Gaps
- Hock (2003)'s own table of degree-day factors and Cuffey & Paterson's A(T) table were not opened; the
  2.4·10⁻²⁴ Pa⁻³ s⁻¹ figure is my unit conversion of the cited 75 MPa⁻³ a⁻¹.
- OGGM's default A and f_s values are not on the page read. Argudo 2020's SIA constants, step sizes and
  timings are missing (paper not readable).
- No source was read for sublimation/aridity limits, calving, or ice-shelf treatment.

## 5. Glacial erosion laws and landforms: simulate or stamp?

### Takeaway
Erosion ∝ (sliding speed)^l with l between 1 and ~2 is the common law; with a decent ice velocity field it
produces U-valleys, hanging valleys, overdeepened lake basins, cirques, arêtes, horns, passes and fjords, but
only together with fluvial, debris-flow and hillslope erosion. Depositional and sub-glacial-water forms
(moraines, drumlins, eskers, kettles, outwash) are not produced by any of the models read and should be
stamped from the simulated ice extent and flow direction.

### Cited Findings
- Abrasion law ∂z/∂t = −k_e ‖u_s‖^l (after Herman et al. 2015) with k_e = 1.3·10⁻⁷ – 1.1·10⁻⁶ m^(1−l)·yr^(l−1)
  and l = 2.02 as I read the parameter table (the PDF text extraction of that table is garbled, so check the
  original). Quarrying ∂z/∂t = −k_q ‖u_s‖ Q(u_s, ∇z) (after Egholm et al. 2012) with k_q = 3.2·10⁻⁵ –
  2.6·10⁻⁴ and Q an erf ramp that concentrates plucking on bed slopes facing down-flow. The ranges express
  rock resistance. Abrasion "carves deeply and enlarges the main glacial valleys"; quarrying "flattens high
  altitudes and produces small lakes just below the ridges". — [Cordonnier et al. 2023, §6.1, Table 1](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- Field calibration: Franz Josef Glacier (NZ) gives erosion ∝ sliding velocity squared
  ([Herman et al. 2015, Science, PubMed record](https://www.pubmed.gov/26450208/)); Patagonia–Antarctic
  Peninsula data give an exponent ≈ 2–2.62 (Koppes et al. 2015); both studies give similar K_g; earlier field
  work suggested a linear law ([Herman, EGU 2016 abstract](https://meetingorganizer.copernicus.org/EGU2016/EGU2016-14074.pdf?pdf)). *(search summaries; K_g values not retrieved)*
- Landforms that *emerge* in Cordonnier 2023 with default settings on a 16 × 16 km, 32 m terrain: U-shaped
  valleys, hanging valleys (tributary ice joins at the ice surface, not at the bed), passes (ice overflowing
  into a neighbour valley breaches the ridge), lakes/overdeepenings both on main valley floors and near
  ridges, cirques (cliff retreat by debris flow above the ice), arêtes and horns where cirques meet; fjords
  when a 1000 m ice cap over low-relief terrain organises into ice streams. — [Cordonnier et al. 2023, §7.1, Fig. 8, Fig. 21](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- Glacial erosion alone gives "excessively steep cliffs, and hydraulically disconnected lakes and valleys";
  fluvial erosion reconnects lakes and notches hanging-valley lips, debris flow cuts the over-steepened walls
  into gullies, hillslope diffusion smooths divides. — [Cordonnier et al. 2023, §7.5, Fig. 5, Fig. 14](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- SIA coupled to erosion fails: without viscous (lateral) stresses the fast ice is too narrow, so erosion
  cuts "deep narrow defiles" (V-shaped) and the bed develops discontinuities that blow up numerically. —
  [Cordonnier et al. 2023, §7.2, Fig. 13](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- Glacially eroded terrain has a hypsometric maximum (most common elevation) just above the ELA — usable as
  a validation metric. — [Cordonnier et al. 2023, §7.2, Fig. 15](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- Maximum mountain heights worldwide track the snowline altitude: glacier sliding destroys topography above
  the snowline while erosional unloading lifts the peaks ("glacial buzzsaw"); contested for large valley
  glaciers that reach far below the ELA. — [Egholm et al. 2009, Nature 460](https://link.springer.com/article/10.1038/nature08263), [Wikipedia: Glacial buzzsaw](https://en.wikipedia.org/wiki/Glacial_buzzsaw) *(search summaries)*
- Not covered by Cordonnier 2023: thermal regime of the ice, sub-glacial hydrology, moraine deposition
  (suggested as future work: abraded rock could parameterise moraines), sedimentation into glacial lakes. —
  [Cordonnier et al. 2023, §7.7, §8](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- In Argudo 2020 moraines, crevasses, seracs, ogives and icefalls are placed by procedural rules on top of
  the SIA result rather than simulated. — [HAL record](https://hal.archives-ouvertes.fr/hal-02929531)
- Glacial stream power (Hergarten 2021): with sliding-dominated flow, E ∝ q_i^θg · S^l with
  θg = (1−α)(ψ−1)/ψ ≈ 0.47 (ψ = 3, width exponent α ≈ 0.3), i.e. the same exponents as the fluvial law, so
  E = K_eff · A^m · S^n with K_eff = γ^m K_g + (1−γ)^m K_f, γ = A_i/A (ice share of the flux), slope taken on
  the ice surface. Erosion is spread over a swath of the glacier width; an effective flux
  A_i,eff = A^0.25 · A_i^0.75 damps parallel flow. Reproduces, at least qualitatively, wide valleys with
  near-flat floors, overdeepenings, hanging valleys and confluence steps; examples use K_g = 1–2 × K_f.
  Limits: valley glaciers only (fails for piedmont and plateau ice), width independent of slope, ad hoc
  swath, lumped K. A referee (F. Beaud) questioned whether a stream-power form is justified for ice. —
  [Hergarten 2021](https://esurf.copernicus.org/articles/9/937/2021/), [discussion](https://esurf.copernicus.org/articles/9/937/2021/esurf-9-937-2021-discussion.html)

### Inferences
- **Simulate** (erosional, valley-scale, emerge from flux + slope): U-shaped troughs, hanging valleys,
  confluence steps, overdeepenings (finger lakes, tarns — fill them with the existing lake code and do not
  silt them up if they are younger than the last glaciation), fjords (troughs cut below sea level: let the
  glacial law keep eroding below sea level while ice is grounded), cirques/arêtes/horns (need a
  headwall-retreat term: threshold-slope erosion above the ice plus a quarrying-like term at ice heads),
  scoured shield (areal low-rate abrasion + plucking under the ice sheet).
- **Stamp** from simulated fields (ice mask at maximum and at retreat stages, flow direction, thickness,
  sediment budget = integrated glacial erosion): terminal and recessional moraines (arcuate ridges 10–50 m
  high along the ice margin at each still-stand; volume ∝ eroded mass delivered to that margin), lateral
  moraines (ridges along the trim line), drumlins (fields of elongated 1:3 ellipsoids, ~10–40 m high, long
  axis along ice flow, behind the margin on low-relief beds), eskers (sinuous ridges following the
  sub-glacial water path = flow routing on the hydraulic potential ≈ ice surface + 0.1 × bed), outwash
  plains (alluvial fan/cone from the margin outlet, low slope — the deposition term can do this if fed the
  glacial sediment), kettle lakes (random pits in outwash and moraine), erratics, roches moutonnées and
  whalebacks (asymmetric bumps, at 16–32 m cells only the largest are resolvable). Dimensions here are
  *recalled, unverified*.
- With n = 1 and the project's dh/dt = U − K·√Q·S, Hergarten's law is a drop-in: compute A_i by the same
  accumulation pass (source term = ice fraction × precipitation, negative below the ELA, clamped at zero),
  swap in K_eff and the ice-surface slope, and widen by a swath stamp. The U-shape then comes from the swath,
  not from physics — acceptable for a game, and avoidable only by the Blatter-class solver.
- If a nonlinear law (l ≈ 2) is wanted for stronger overdeepening, apply it as an explicit correction after
  the implicit n = 1 solve; glacial rates are small per step (k_e·u² with u ~ 10–100 m/yr gives ~0.01–10
  mm/yr from the cited k_e range — my arithmetic).

### Gaps
- No quantitative morphometry of drumlins, eskers, moraines or kettle fields was sourced.
- K_g values of Herman 2015 / Koppes 2015, and Deal & Prasicek's closed-form law, were not retrieved.
- Uncertain reading of the exponent l (2.02) in Cordonnier's Table 1 because of PDF text extraction.

## 6. Imprinting a past glaciation (last glacial maximum) and retreating to the present

### Takeaway
Run the climate as a schedule of the ELA (or sea-level temperature): lower it by roughly 900–1000 m for a
long cold stage, let glacial erosion act, then raise it quickly and let fluvial/debris/hillslope processes
partly repair the landscape. One cycle does most of the work; later cycles add little.

### Cited Findings
- Scenario used to make the paper's results: 3 Myr of fluvial-only evolution of a fractal terrain at
  Δt = 25,000 yr; then cooling, E lowered from 3000 m (summit level) to 1200 m over 100,000 yr with
  Δt = 100 yr; then fast warming, E up to 1700 m in 20,000 yr so other processes smooth glacial forms. A
  second glaciation of the same magnitude has reduced erosional impact, "since glacial erosion carves most
  of the relief above the snowline altitude". Roughly 2000 steps in total. — [Cordonnier et al. 2023, §7](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- Colder climates extend the glaciated valleys, wetter ones deepen the erosion pattern. — [Cordonnier et al. 2023, Fig. 16](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- LGM ELA lowering varied geographically with a mid-range value of ~900 m (≈ 780 m when measured against
  present sea level, because sea level itself fell) ([Quaternary Research, sea-level effect on ELA depression](https://cambridge.org/core/journals/quaternary-research/article/effect-of-sealevel-lowering-on-ela-depression-during-the-lgm/9E0BFA4886EE7BE4D4A617AEDD04D480));
  southern tropical Andes 920 ± 250 m, implying 5.4 °C cooling, or 4.7 °C after correcting for a ~120 m
  sea-level fall ([Snowline depression in the tropics, record](https://data.zhenzhi.com/details/1/875340571)). *(search summaries)*
- Sea-level temperature differs by up to 10 °C between glacial and interglacial periods in some locations. —
  [Cordonnier et al. 2023, §7.4](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- Ice-cap case: a mostly flat topography under 1000 m of ice develops ice streams that carve deep fjords
  during the simulation and retreat. — [Cordonnier et al. 2023, Fig. 21](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)

### Inferences
- **Generator schedule (mine):** after the tectonic/fluvial history, run one glacial stage: global
  temperature offset −5 to −8 °C (ELA ≈ −900 m by the 6.5 K/km lapse rate, more at high latitude), sea level
  −120 m; compute the LGM ice mask (ice sheet on the coarse grid, valley glaciers by routing); apply glacial
  erosion for an *effective* duration (the Quaternary had tens of cycles; since later cycles add little, a
  single stage with boosted K_g is a fair stand-in); then step the temperature back in 3–5 stages, stamping
  a recessional moraine at each still-stand; finish with a short (10–20 kyr) fluvial + threshold-slope +
  diffusion pass that must be too short to erase the lakes.
- **What makes mid-latitude land read as "formerly glaciated":** (1) thousands of unfilled lakes — skip lake
  silting inside the LGM mask and keep overdeepenings; (2) deranged drainage — inside the mask, rebuild the
  drainage tree on the scoured, till-plastered surface instead of the inherited fluvial one; (3) stripped
  soil/regolith on shields (bare rock material at the surface) and thick till/outwash at the margin (a soil
  depth map: ~0 in the scour zone, metres to tens of metres in the marginal belt); (4) fjords where troughs
  reach the coast, flooded when sea level returns; (5) isostatic depression under the ice (~1/3–1/4 of ice
  thickness for a mantle/ice density ratio of ~3.6 — my arithmetic) with incomplete rebound today, giving
  raised beaches and shallow epicontinental seas like Hudson Bay/the Baltic if the generator wants them;
  (6) loess belts and dune fields downwind of outwash (stamp).
- Present ice extent is then just the mass-balance evaluation at today's temperature; the difference between
  the LGM mask and today's mask is the "glacial landform zone" used by the stamping pass and by biome/soil
  logic.

### Gaps
- No source was read on ice-sheet extent as a function of latitude/precipitation (why Laurentide and
  Fennoscandia but not most of Siberia), on glacial isostatic adjustment magnitudes, or on proglacial lake
  and spillway (megaflood) landforms.
- No published "single boosted cycle equals many cycles" equivalence was found; that is my simplification
  based on the cited diminishing effect of the second glaciation.

## 7. Other erosion processes worth adding at 16–32 m cells

### Takeaway
The cited glacial paper itself shows the minimum set that makes a mountain landscape look right at 32 m:
fluvial incision, a slope-threshold/debris-flow term with a critical angle near 37°, and hillslope diffusion;
sediment deposition (Yuan 2019) is the next most valuable because it creates fans, floodplains and basin fill
and also slows the decay of old ranges.

### Cited Findings
- Thermal erosion as a slope clamp: if a stream-power update would create a slope above 30° ("the usual
  talus angle"), reduce the elevation change to keep the slope in range; a uniform 30° limit gives "very
  regular geometric valleys", so the limit is varied by 3D noise between 6° and 54° to mimic rock strength,
  or made height-dependent to put cliffs on crests. — [Cordonnier et al. 2016, §5–6](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonier16CGF.pdf)
- Debris-flow law (after Stock & Dietrich 2003): ∂z/∂t = −k_df (1 + k_da A^q) · max(0, ‖∇z‖ − s_c)^q′ with
  k_df = 10⁻⁴ – 2.5·10⁻⁴, k_da = 10, q = 0.8, q′ = 2.0, critical slope s_c = 37°; solved explicitly with the
  same drainage ordering as the fluvial term; it cuts the steepest slopes into dendritic gullies with sharp
  ridges and is what turns glacial headwalls into cirques. Fluvial: k_f = 1.5·10⁻⁶ – 5·10⁻⁶ m^(1−2m)/yr,
  m = 0.4. Hillslope diffusion: k_h = 1·10⁻² – 5·10⁻² m²/yr, solved with an ADI implicit scheme for long
  steps. — [Cordonnier et al. 2023, §6.2–6.4, Table 1](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- Landslide erosion rate, not hillslope angle, is what rises with uplift once slopes reach a threshold;
  one regional estimate of the threshold angle is 30 ± 5° (SE Tibet); landsliding and rock strength cap
  relief (Schmidt & Montgomery 1995). *(search summaries)* — [Larsen & Montgomery 2012, Nature Geoscience](https://www.nature.com/ngeo/journal/v5/n7/full/ngeo1479.html)
- Material-specific slides: separate rock-, sand- and humus-slide events, each with its own friction angle;
  a slide moves the part of the granular column above the friction angle downslope until the local slope
  falls below it; bedrock fractures into loose rock at a rate ∝ temperature variation × slope, reduced by
  granular and vegetation cover. — [Cordonnier et al. 2017, §4](https://cs.purdue.edu/homes/bbenes/papers/Cordonnier17ToG.pdf)
- Sediment transport and deposition: the Yuan et al. 2019 scheme is O(N), implicit and unconditionally
  stable ([ADS record](https://ui.adsabs.harvard.edu/abs/2019JGRF..124.1346Y)); natural fan systems give
  G > 0.4 ([Guérit et al. 2019](https://typeset.io/pdf/fluvial-landscape-evolution-controlled-by-the-sediment-1hefaht0hi.pdf));
  FastScape's sediment model keeps bedrock and soil as separate layers with their own K, G and diffusivity,
  and its marine model spreads sand and silt offshore by diffusion with compaction
  ([FastScape models](https://fastscape.readthedocs.io/en/latest/models.html)).
- Tzathas 2024 adds landslides and hillslope processes to the analytical stream-power solution. *(abstract)*
  — [EG digital library](https://diglib.eg.org/handle/10.1111/cgf15033)
- Wind erosion, differential weathering and sandstone forms have a dedicated 3D simulator (Arenite 2025). —
  [Yang et al. 2025](https://cs.purdue.edu/homes/bbenes/papers/Yang25ToG.pdf)

### Inferences
- **Priority list for this generator (mine):** (1) rock-dependent threshold slope with a talus layer —
  bedrock above its stability angle (hard massive rock 45–70°, weak rock 25–35°) sheds material that rests
  at ~33–37°; this single pass creates cliffs-over-scree, the signature of young ranges, and its absence is
  why pure stream power + diffusion looks "soft"; (2) Yuan-style deposition with G ≈ 0.5–1 → alluvial fans at
  range fronts, braided valley fills, intramontane basins, deltas where rivers reach lakes/sea (existing lake
  silting becomes a special case); (3) the drainage-weighted debris-flow term above for gullied headwalls;
  (4) coastal: a wave-cut notch/platform pass (erode cells within a few metres of sea level proportional to
  fetch-based wave energy and rock K; cliffs retreat by the threshold-slope pass; move sand alongshore into
  beaches, spits and barrier islands) — stamp-level is enough; (5) aeolian: only in arid cells with sand
  supply; stamp dune fields (barchan/transverse/linear/star chosen from wind-direction variability and sand
  supply) rather than simulate saltation; (6) karst: see section 2.
- Soil-mantled (old, slow) hillslopes are convex with diffusion; threshold (young, fast) hillslopes are
  planar at the critical angle. Making diffusivity and the threshold pass depend on the local erosion rate
  and soil cover automatically ties ridge sharpness to belt age.
- All of these are O(n) per pass with small constants (a threshold-slope sweep in topological order, one
  ADI or multigrid diffusion solve), so they do not change the cost class of the generator.

### Gaps
- No primary sources were read for coastal erosion, aeolian dune models or karst models; angle-of-repose
  values per rock type are from memory.
- Stock & Dietrich's debris-flow law was read only through Cordonnier 2023's use of it; the units of k_df
  and k_da in the garbled table should be re-checked in the PDF.

## 8. Costs, numerical stability and published run times

### Takeaway
Everything fluvial is O(n) per implicit step and needs only 100–300 steps; glacial erosion with a real ice
solver costs ~2000 steps and minutes on a GPU at 1–4 million cells; the stream-power glacial law costs
1.7–2.7× a fluvial step and has been run on 25 million cells. A CPU-only, minutes-long budget therefore fits
the routing approach comfortably and the Cordonnier 2023 approach not at all without a GPU and a network.

### Cited Findings
- Stream power, implicit, graph-based (2016): 0.031 s/step at 10k nodes to 1.273 s/step at 160k nodes on one
  3 GHz CPU; ~200 steps to converge (6.4 s to 252 s); δt = 2.5·10⁵ yr. — [Cordonnier et al. 2016, Table 1](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonier16CGF.pdf)
- 2018 pipeline: 4 s per erosion step at 1,000,000 vertices, 100 iterations for a final terrain. —
  [Cordonnier et al. 2018, §7](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier18TVCG.pdf)
- Glacial erosion (2023), PyTorch on a 16-core Xeon E5-2650 + 24 GB Nvidia RTX GPU: one step at 1024² is
  300 ms (87% ice-flow network inference, 3% ice transport, 10% mass balance and erosion); drainage on the
  CPU takes 1.7 s and is refreshed every 20 GPU steps; ≈ 2000 steps per result: 3.7 min at 512², 9.8 min at
  1024², 37.5 min at 2048²; CPU drainage and transfers are almost half of the total. The ice-flow emulator is
  a 6-layer, 64-feature CNN trained on the fly without a dataset; a direct optimisation of the Blatter energy
  needs ~100 iterations of 130 ms (50× slower); an SIA step is 4.7 ms but needs ~100× smaller steps at 32 m.
  Domains 16 × 16 km at 32 m and 64 × 64 km. — [Cordonnier et al. 2023, §7, §7.3](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- Time-step limits: explicit ice advection Δt < 0.1·Δx/‖ū‖ (≈ 0.1 yr at 100 m, 100 m/yr); their
  multi-scale scheme caps the local step at δt(x) = min(dt, c·Δx/‖ū(x)‖) and advances Gaussian-pyramid
  residuals with a scale factor f = 1.7 to reach dt = 10–100 yr. — [Cordonnier et al. 2023, §5](https://www.cs.purdue.edu/homes/bbenes/papers/Cordonnier23ToG.pdf)
- Explicit SIA: Δt ≤ 2R / [D(1/Δx² + 1/Δy²)] — quadratic in cell size, inverse in diffusivity. —
  [PISM manual](https://pism.io/docs/manual/modeling-choices/dynamics/sia.html)
- Glacial stream power (OpenLEM): fully implicit, cost linear in node count; 5000 × 5000 grids (50 m
  spacing in the physical reading); fluvio-glacial step costs 1.7× the fluvial step with no ice and 2.7×
  when fully glaciated; time steps of hundreds to thousands of years; "several million years on lattices of
  several million nodes on standard PCs". Flow directions, surface and ice thickness are lagged explicitly,
  which limits the step when climate changes fast. No absolute wall-clock times are given. — [Hergarten 2021](https://esurf.copernicus.org/articles/9/937/2021/)
- Implicit Braun–Willett stepping is stable for any Δt and exact at steady state, but transient accuracy
  depends on Δt. — [Landlab docs](https://landlab.readthedocs.io/en/latest/generated/api/landlab.components.stream_power.fastscape_stream_power.html)
- Deposition (Yuan 2019): O(N), implicit, unconditionally stable. — [ADS record](https://ui.adsabs.harvard.edu/abs/2019JGRF..124.1346Y)
- OGGM flowline glaciers: semi-implicit scheme 1.5× cost per step, 3× larger step. — [OGGM ice dynamics](https://docs.oggm.org/en/stable/ice-dynamics.html)

### Inferences
- **Budget sketch for a 4096² fine level (16.8 M cells), CPU, my estimate:** an O(n) fluvial step with
  priority-flood and tree ordering is on the order of 1–3 s single-threaded at that size (extrapolating the
  2016 graph numbers, ~8 µs/node, which were unoptimised); a glacial stage of 200–500 steps at 2–3× that cost
  on the *fine* grid would be 10–60 min — too slow, so run the glacial stage on a 2–4× coarser level (64–128 m
  cells, where valley glaciers of 0.5–3 km width are still resolved) and let the multigrid prolongation plus
  a few fine fluvial/threshold steps add detail. That brings it to tens of seconds to a few minutes.
- Ice sheets on the planetary grid are negligible (a 512² grid, a few thousand explicit SIA steps or a
  closed-form plastic profile).
- Stability ranking for a generator that wants big steps: implicit stream power (unconditional) >
  implicit/ADI diffusion (unconditional) > Hergarten glacial law (implicit erosion, lagged ice geometry:
  keep the ELA change per step small) > threshold-slope sweeps (iterate to convergence, always stable) >
  explicit debris-flow term (needs Δt·k·slope^q′ small; sub-step it) > explicit SIA (avoid at fine cells).
- Determinism: all of the above are plain grid sweeps in a fixed topological order, so they can be made
  bit-reproducible across threads if accumulation order is fixed (tiles reduced in a fixed order) and libm
  is used for transcendentals — relevant to this project's "pure function of coordinates" worldgen rule.

### Gaps
- No published CPU timings for FastScape/fastscapelib at multi-million-cell grids were retrieved in this
  session, and none for Schott 2023, Tzathas 2024 or Argudo 2020.
- The per-node cost extrapolation and the whole budget sketch are estimates, not measurements.
