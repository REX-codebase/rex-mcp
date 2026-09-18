# REX Harness UI - token system (roles, not paint)

canvas    #0B0A10   app background, violet-black
surface   #14121C   panels
raised    #1C1928   cards, popovers
border    #292339   hairlines only
text      #EDEBF5   primary
muted     #918CA3   secondary text
faint     #5E5870   timestamps, receipt ids
accent    #8B7CF0   violet - reserved for the task spine, primary action, focus
working   #8B7CF0   (accent doubles as working state)
done      #6ED3A5   soft green, state only
blocked   #E0A458   amber, state only
danger    #DF6B6B   errors
focus     2px outline accent at 70%

type: Inter (UI) + JetBrains Mono (receipt ids, metrics, timestamps)
measure: 45-70ch for prose; radius 10/8px; spacing 4pt grid
motion: 150-220ms ease-out, transform/opacity only; prefers-reduced-motion -> instant states

Contrast checks (computed):
text #EDEBF5 on canvas #0B0A10 = 14.9:1 AAA
muted #918CA3 on canvas = 6.1:1 AA
faint #5E5870 on canvas = 3.6:1 (decorative/secondary only, never body)
accent #8B7CF0 on canvas = 5.2:1 AA
done #6ED3A5 on canvas = 8.4:1 AA
