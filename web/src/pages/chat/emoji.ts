// Набор эмодзи для поля ввода: разделы как в привычных мессенджерах, поиск по словам (русским и английским),
// «Недавние» — в этом браузере. Флаги не включены: в Windows они показываются буквами.

export type EmojiGroupKey = "smileys" | "people" | "nature" | "food" | "activity" | "travel" | "objects" | "symbols";
export type EmojiGroup = { key: EmojiGroupKey; icon: string; items: { e: string; words: string }[] };

const RAW: [EmojiGroupKey, string, string][] = [
  [
    "smileys",
    "😀",
    `😀 улыбка радость smile grin
😃 улыбка радость smile happy
😄 смех радость laugh happy
😁 улыбка зубы grin
😆 смех хохот laugh
😅 пот неловко sweat
🤣 смех катаюсь rofl
😂 слёзы смех joy lol
🙂 улыбка slight smile
🙃 наоборот upside down
😉 подмигивание wink
😊 румянец доволен blush
😇 ангел невинный angel
🥰 влюблён любовь love
😍 влюблён глаза сердца heart eyes
🤩 звёзды восторг star struck
😘 поцелуй kiss
😗 поцелуй kiss
😚 поцелуй kiss
😋 вкусно yum
😛 язык tongue
😜 подмигивание язык wink tongue
🤪 безумный crazy zany
😝 язык tongue
🤑 деньги money
🤗 обнимашки hug
🤭 ой хихи oops giggle
🤫 тихо секрет shh quiet
🤔 думаю хм thinking
🤐 молчу молния zipper
🤨 сомнение бровь raised eyebrow
😐 нейтрально neutral
😑 без выражения expressionless
😶 нет слов no mouth
😏 ухмылка smirk
😒 недоволен unamused
🙄 закатил глаза eye roll
😬 неловко гримаса grimace
🤥 враньё lie
😌 облегчение relieved
😔 грусть задумчивый pensive
😪 сонный sleepy
🤤 слюни drool
😴 сплю сон sleep
😷 маска болею mask sick
🤒 температура болею sick
🤕 травма бинт hurt
🤢 тошнит nauseated
🤮 рвота vomit
🤧 чих простуда sneeze
🥵 жарко hot
🥶 холодно cold
🥴 пьяный woozy
😵 головокружение dizzy
🤯 взрыв мозга mind blown
🤠 ковбой cowboy
🥳 праздник вечеринка party
😎 крутой очки cool sunglasses
🤓 ботаник nerd
🧐 монокль присмотрелся monocle
😕 растерян confused
😟 беспокойство worried
🙁 грусть frown
😮 удивление wow surprised
😯 удивление hushed
😲 шок astonished
😳 смущение flushed
🥺 умоляю please pleading
😦 испуг frowning
😧 тревога anguished
😨 страх fearful
😰 тревога пот anxious
😥 грусть облегчение sad relieved
😢 плачу слеза cry
😭 рыдаю sob
😱 ужас крик scream
😖 мучение confounded
😣 упорство persevere
😞 разочарован disappointed
😓 пот усталость sweat
😩 устал weary
😫 устал tired
🥱 зевок скучно yawn
😤 пар злость triumph
😡 злость ярость angry rage
😠 злой angry
🤬 мат ругань cursing
😈 чертёнок devil
👿 злой чёрт imp
💀 череп skull dead
💩 какашка poop
🤡 клоун clown
👻 призрак ghost
👽 инопланетянин alien
🤖 робот robot
😺 кот улыбка cat
😸 кот cat
😹 кот смех cat joy
😻 кот любовь cat love
🙈 не вижу обезьяна see no evil
🙉 не слышу hear no evil
🙊 молчу speak no evil`,
  ],
  [
    "people",
    "👍",
    `👋 привет пока wave hello
🤚 рука стоп raised hand
✋ рука стоп hand
🖖 вулкан vulcan
👌 окей ок ok
🤌 итальянский pinched
🤏 чуть-чуть pinch
✌️ мир победа peace victory
🤞 скрестил пальцы удача fingers crossed luck
🤟 люблю тебя love you
🤘 рок rock
🤙 позвони call me
👈 налево left
👉 направо right
👆 вверх up
👇 вниз down
☝️ внимание один index up
👍 да класс лайк like yes thumbs up
👎 нет дизлайк dislike no thumbs down
✊ кулак fist
👊 удар кулак punch
🤛 кулак fist
🤜 кулак fist
👏 аплодисменты браво clap
🙌 ура hooray raised hands
👐 открытые руки open hands
🤲 ладони palms
🤝 рукопожатие договорились handshake deal
🙏 спасибо пожалуйста молюсь thanks please pray
✍️ пишу write
💅 маникюр nails
💪 сила мышцы strong muscle
🧠 мозг brain
👀 смотрю глаза eyes look
👁️ глаз eye
👄 губы lips
🫶 сердце руками heart hands
👶 малыш baby
🧒 ребёнок child
👦 мальчик boy
👧 девочка girl
🧑 человек person
👨 мужчина man
👩 женщина woman
🧓 пожилой older
👮 полиция police
💂 охрана guard
🕵️ детектив detective
👷 строитель builder
👨‍💻 программист айтишник developer
👩‍💻 программистка айтишник developer
👨‍🔧 механик техник mechanic
👩‍🔧 техник mechanic
👨‍💼 офис менеджер office
👩‍💼 офис менеджер office
🧑‍🏫 учитель teacher
🧑‍🚀 космонавт astronaut
🦸 супергерой superhero
🧙 маг wizard
🙋 я рука вверх raising hand
🙆 хорошо ok gesture
🙅 нельзя no gesture
🤷 не знаю пожимаю плечами shrug
🤦 фейспалм facepalm
🙇 поклон извините bow sorry
💁 подсказка tipping hand
🏃 бегу run
🚶 иду walk
🧘 медитация йога yoga
🛌 сплю bed sleep
👫 пара couple
👪 семья family
🗣️ говорю speaking
👥 люди команда people team`,
  ],
  [
    "nature",
    "🐱",
    `🐶 собака пёс dog
🐱 кошка кот cat
🐭 мышь mouse
🐹 хомяк hamster
🐰 кролик заяц rabbit
🦊 лиса fox
🐻 медведь bear
🐼 панда panda
🐨 коала koala
🐯 тигр tiger
🦁 лев lion
🐮 корова cow
🐷 свинья pig
🐸 лягушка frog
🐵 обезьяна monkey
🐔 курица chicken
🐧 пингвин penguin
🐦 птица bird
🦆 утка duck
🦅 орёл eagle
🦉 сова owl
🐺 волк wolf
🐴 лошадь horse
🦄 единорог unicorn
🐝 пчела bee
🐛 жук bug
🦋 бабочка butterfly
🐌 улитка медленно snail slow
🐞 божья коровка ladybug
🐢 черепаха медленно turtle
🐍 змея snake
🐙 осьминог octopus
🐠 рыба fish
🐬 дельфин dolphin
🐳 кит whale
🦈 акула shark
🐘 слон elephant
🦒 жираф giraffe
🦔 ёж hedgehog
🐾 следы лапы paws
🌵 кактус cactus
🎄 ёлка новый год tree christmas
🌲 ель tree
🌳 дерево tree
🌴 пальма отпуск palm
🌱 росток seedling
🌿 трава herb
🍀 клевер удача clover luck
🍁 клён осень maple autumn
🍂 листья осень leaves
💐 букет bouquet
🌷 тюльпан tulip
🌹 роза rose
🌸 сакура цветок blossom
🌼 цветок flower
🌻 подсолнух sunflower
🌞 солнце sun
🌝 луна moon
🌙 месяц ночь moon night
⭐ звезда star
🌟 звезда сияет star glow
✨ блеск искры sparkles
⚡ молния быстро lightning zap
🔥 огонь круто fire hot lit
🌈 радуга rainbow
☀️ солнечно sunny
⛅ облачно cloudy
🌧️ дождь rain
⛈️ гроза storm
❄️ снег зима snow winter
☃️ снеговик snowman
🌊 волна wave
💧 капля drop
☔ зонт дождь umbrella rain`,
  ],
  [
    "food",
    "🍕",
    `🍏 яблоко apple
🍎 яблоко apple
🍐 груша pear
🍊 апельсин orange
🍋 лимон lemon
🍌 банан banana
🍉 арбуз watermelon
🍇 виноград grapes
🍓 клубника strawberry
🍒 вишня cherry
🍑 персик peach
🥭 манго mango
🍍 ананас pineapple
🥥 кокос coconut
🥝 киви kiwi
🍅 помидор tomato
🥑 авокадо avocado
🥦 брокколи broccoli
🥒 огурец cucumber
🌶️ перец острый pepper hot
🌽 кукуруза corn
🥕 морковь carrot
🥔 картошка potato
🥐 круассан croissant
🍞 хлеб bread
🥖 багет baguette
🧀 сыр cheese
🥚 яйцо egg
🍳 яичница завтрак breakfast egg
🥞 блины pancakes
🥓 бекон bacon
🍗 курица chicken
🍖 мясо meat
🌭 хот-дог hotdog
🍔 бургер burger
🍟 картошка фри fries
🍕 пицца pizza
🥪 бутерброд sandwich
🌮 тако taco
🥗 салат salad
🍝 паста макароны pasta
🍜 лапша суп ramen noodles
🍲 суп soup
🍣 суши sushi
🍱 обед ланч bento lunch
🥟 пельмени dumplings
🍤 креветка shrimp
🍚 рис rice
🍦 мороженое ice cream
🍩 пончик donut
🍪 печенье cookie
🎂 торт день рождения cake birthday
🍰 торт пирожное cake
🧁 капкейк cupcake
🍫 шоколад chocolate
🍬 конфета candy
🍭 леденец lollipop
🍯 мёд honey
☕ кофе coffee
🍵 чай tea
🧃 сок juice
🥤 напиток drink
🧋 чай с молоком bubble tea
🍺 пиво beer
🍻 пиво тост cheers beers
🥂 шампанское тост cheers
🍷 вино wine
🥃 виски whisky
🍸 коктейль cocktail
🍾 праздник шампанское champagne
🧊 лёд ice`,
  ],
  [
    "activity",
    "⚽",
    `⚽ футбол football soccer
🏀 баскетбол basketball
🏈 регби football
⚾ бейсбол baseball
🎾 теннис tennis
🏐 волейбол volleyball
🏓 пинг-понг ping pong
🏸 бадминтон badminton
🥅 ворота goal
🏒 хоккей hockey
⛳ гольф golf
🎣 рыбалка fishing
🎽 бег спорт running
🎿 лыжи ski
🛷 санки sled
⛸️ коньки skate
🏆 кубок победа trophy win
🥇 первое место золото gold first
🥈 второе место серебро silver
🥉 третье место бронза bronze
🏅 медаль medal
🎖️ награда medal
🎗️ лента ribbon
🎫 билет ticket
🎟️ билеты tickets
🎪 цирк circus
🎭 театр theater
🎨 рисование искусство art
🎬 кино movie
🎤 микрофон караоке mic
🎧 наушники музыка headphones
🎼 ноты музыка music
🎹 пианино piano
🥁 барабан drum
🎸 гитара guitar
🎮 игра геймпад game
🕹️ джойстик joystick
🎲 кубик игра dice
♟️ шахматы chess
🧩 пазл puzzle
🎯 цель мишень target goal
🎳 боулинг bowling
🎉 праздник ура party tada
🎊 конфетти confetti
🎈 шарик balloon
🎁 подарок gift present
🎀 бант ribbon
🪅 пиньята pinata
🎆 салют fireworks
🎇 бенгальский огонь sparkler`,
  ],
  [
    "travel",
    "🚗",
    `🚗 машина авто car
🚕 такси taxi
🚙 внедорожник suv
🚌 автобус bus
🚎 троллейбус trolleybus
🏎️ гонка race car
🚓 полиция police car
🚑 скорая ambulance
🚒 пожарные fire engine
🚚 грузовик доставка truck delivery
🚜 трактор tractor
🚲 велосипед bike
🛴 самокат scooter
🏍️ мотоцикл motorcycle
🚨 мигалка тревога alarm siren
🚦 светофор traffic light
🚧 ремонт стройка construction
⛽ заправка fuel
🚂 поезд train
🚆 поезд train
🚇 метро metro subway
🚊 трамвай tram
✈️ самолёт командировка plane flight
🛫 вылет departure
🛬 прилёт arrival
🚀 ракета запуск rocket launch
🛸 нло ufo
🚁 вертолёт helicopter
⛵ парусник sailboat
🚢 корабль ship
⚓ якорь anchor
🗺️ карта map
🧭 компас compass
🏔️ горы mountain
🏕️ кемпинг camping
🏖️ пляж отпуск beach vacation
🏝️ остров отпуск island
🏠 дом house home
🏡 дом сад house garden
🏢 офис здание office building
🏬 торговый центр mall
🏭 завод factory
🏥 больница hospital
🏦 банк bank
🏨 отель hotel
🏫 школа school
🏛️ здание музей museum
⛪ церковь church
🗼 башня tower
🌉 мост bridge
🌃 ночной город night city
🌇 закат sunset
🎡 колесо обозрения ferris wheel
🎢 горки roller coaster`,
  ],
  [
    "objects",
    "💡",
    `⌚ часы watch
📱 телефон смартфон phone mobile
💻 ноутбук laptop
⌨️ клавиатура keyboard
🖥️ компьютер монитор desktop computer
🖨️ принтер printer
🖱️ мышь mouse
💾 дискета сохранить save floppy
💿 диск cd
📀 dvd
🧮 счёты abacus
📷 фото камера camera photo
📸 фото вспышка camera flash
📹 видео video
🎥 кино камера movie camera
📞 телефон звонок phone call
☎️ телефон phone
📟 пейджер pager
📠 факс fax
📺 телевизор tv
📻 радио radio
🎙️ микрофон подкаст microphone
⏰ будильник время alarm clock
⏳ ждём время hourglass wait
⌛ время вышло hourglass done
📡 антенна antenna
🔋 батарея battery
🪫 батарея села low battery
🔌 вилка розетка plug
💡 идея лампа idea bulb
🔦 фонарь flashlight
🕯️ свеча candle
🧯 огнетушитель extinguisher
🗑️ мусор корзина trash
💸 деньги улетают money spend
💵 доллары деньги dollar money
💶 евро euro
💰 мешок денег money bag
💳 карта оплата card payment
🧾 чек квитанция receipt
✉️ письмо конверт email letter
📧 почта email
📨 входящее incoming
📩 письмо envelope
📤 исходящие outbox
📥 входящие inbox
📦 посылка коробка package box
📫 почтовый ящик mailbox
📝 заметка записать memo note
📄 документ страница document page
📃 документ page
📑 закладки bookmark tabs
📊 диаграмма отчёт chart report
📈 рост график chart up growth
📉 падение график chart down
🗒️ блокнот notepad
📆 календарь calendar
📅 дата календарь date calendar
🗓️ календарь calendar
📇 картотека контакты card index
📋 буфер список clipboard list
📁 папка folder
📂 открытая папка open folder
🗂️ разделители dividers
🗃️ архив archive
🗄️ шкаф cabinet
📌 кнопка закрепить pin pushpin
📍 место метка location pin
📎 скрепка вложение paperclip attachment
🖇️ скрепки paperclips
📏 линейка ruler
📐 угольник triangle ruler
✂️ ножницы scissors
🖊️ ручка pen
🖋️ перо pen
✏️ карандаш pencil edit
🔍 поиск лупа search
🔎 поиск лупа search
🔒 замок закрыто lock locked
🔓 открыто unlock
🔐 замок ключ lock key
🔑 ключ пароль key password
🗝️ старый ключ old key
🔨 молоток hammer
🪛 отвёртка screwdriver
🔧 ключ гаечный ремонт wrench fix
🔩 болт bolt
⚙️ шестерёнка настройки gear settings
🧰 инструменты toolbox
🧲 магнит magnet
🪜 лестница ladder
🧪 пробирка тест test tube
🔬 микроскоп microscope
🔭 телескоп telescope
💊 таблетка pill
🩹 пластырь bandage
🩺 стетоскоп врач stethoscope
🚪 дверь door
🪑 стул chair
🛋️ диван couch
🚽 туалет toilet
🧹 веник уборка broom
🧺 корзина basket
🧻 бумага paper roll
🛒 тележка покупки cart shopping
🎒 рюкзак backpack
👓 очки glasses
💼 портфель работа briefcase work
👔 галстук tie
👕 футболка tshirt
👟 кроссовки sneakers
👑 корона crown king
💍 кольцо ring
💎 бриллиант gem diamond
☂️ зонт umbrella`,
  ],
  [
    "symbols",
    "❤️",
    `❤️ сердце любовь heart love
🧡 оранжевое сердце orange heart
💛 жёлтое сердце yellow heart
💚 зелёное сердце green heart
💙 синее сердце blue heart
💜 фиолетовое сердце purple heart
🖤 чёрное сердце black heart
🤍 белое сердце white heart
🤎 коричневое сердце brown heart
💔 разбитое сердце broken heart
❣️ сердце heart
💕 сердечки hearts
💞 сердечки hearts
💓 сердце бьётся heartbeat
💗 сердце растёт growing heart
💖 сердце блеск sparkling heart
💘 сердце стрела cupid
💝 сердце подарок heart gift
💯 сто процентов отлично hundred
💢 злость anger
💥 бум взрыв boom
💫 головокружение dizzy
💦 капли пот sweat drops
💨 быстро dash
💬 сообщение чат speech chat
💭 мысль thought
🗯️ крик anger bubble
💤 сон zzz sleep
✅ готово галочка done check yes
☑️ отмечено check box
✔️ галочка check
❌ нет ошибка крест no cross error
❎ крест cross
➕ плюс plus
➖ минус minus
➗ делить divide
✖️ умножить multiply
❓ вопрос question
❔ вопрос question
❗ восклицание важно exclamation important
❕ восклицание exclamation
‼️ двойное восклицание double exclamation
⁉️ вопрос восклицание interrobang
⚠️ внимание предупреждение warning
🚫 запрещено нельзя forbidden
⛔ стоп нет входа no entry
⭕ круг circle
🛑 стоп stop
🔴 красный red circle
🟠 оранжевый orange circle
🟡 жёлтый yellow circle
🟢 зелёный green circle
🔵 синий blue circle
🟣 фиолетовый purple circle
⚫ чёрный black circle
⚪ белый white circle
🟥 красный квадрат red square
🟩 зелёный квадрат green square
🟦 синий квадрат blue square
🔶 ромб diamond
🔷 ромб diamond
🔺 треугольник triangle
🔻 треугольник вниз triangle down
▶️ играть play
⏸️ пауза pause
⏹️ стоп stop
⏺️ запись record
⏭️ далее next
⏮️ назад previous
🔁 повтор repeat
🔄 обновить refresh
🔃 обновить reload
🔀 перемешать shuffle
⬆️ вверх up
⬇️ вниз down
⬅️ влево left
➡️ вправо right
↩️ вернуть back
↪️ вперёд forward
🔝 топ top
🆕 новое new
🆗 ок ok
🆙 вверх up
🆒 круто cool
🆓 бесплатно free
🆘 помощь sos help
ℹ️ информация info
🔔 колокольчик уведомление bell
🔕 без звука mute bell
📢 объявление громко loudspeaker
📣 мегафон megaphone
🔇 без звука mute
🔊 громко loud
♻️ переработка recycle
✳️ звёздочка asterisk
❇️ искра sparkle
©️ копирайт copyright
®️ зарегистрировано registered
™️ товарный знак trademark
#️⃣ решётка hash
0️⃣ ноль zero
1️⃣ один one
2️⃣ два two
3️⃣ три three
4️⃣ четыре four
5️⃣ пять five
🔟 десять ten
🕐 час время clock
🏳️ белый флаг white flag
🏴 чёрный флаг black flag
🏁 финиш finish
🚩 флажок flag`,
  ],
];

export const EMOJI_GROUPS: EmojiGroup[] = RAW.map(([key, icon, list]) => ({
  key,
  icon,
  items: list.split("\n").map((line) => {
    const sp = line.indexOf(" ");
    return { e: line.slice(0, sp), words: line.slice(sp + 1).toLowerCase() };
  }),
}));

const ALL = EMOJI_GROUPS.flatMap((g) => g.items);

/** Поиск: каждое слово запроса — начало одного из слов эмодзи. */
export function searchEmoji(q: string, limit = 64): string[] {
  const terms = q.toLowerCase().split(/\s+/).filter(Boolean);
  if (terms.length === 0) return [];
  const out: string[] = [];
  for (const x of ALL) {
    const words = x.words.split(" ");
    if (terms.every((t) => words.some((w) => w.startsWith(t)))) {
      out.push(x.e);
      if (out.length >= limit) break;
    }
  }
  return out;
}

const RECENT_KEY = "pult.chat.emoji.recent";
const RECENT_MAX = 24;

export function recentEmoji(): string[] {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string").slice(0, RECENT_MAX) : [];
  } catch {
    return [];
  }
}

export function rememberEmoji(e: string) {
  try {
    const next = [e, ...recentEmoji().filter((x) => x !== e)].slice(0, RECENT_MAX);
    localStorage.setItem(RECENT_KEY, JSON.stringify(next));
  } catch {
    // Хранилище недоступно (приватный режим) — «Недавние» просто не запоминаются.
  }
}
