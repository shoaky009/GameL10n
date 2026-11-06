"""Verified serialized display fields; identifiers and opaque tails stay intact."""
import json
from serialized_file import Reader, unity_string


def unit(path, pid, field, text, name, script=None, kind='dialogue'):
    if script is None:
        return dict(id=f'{path}::{pid}::{field}', resourcePath=path,
                    locator=f'TextAsset:{pid}/{name}/{field}', sourceText=text,
                    context=f'{name} / {field}', speaker=None, kind=kind)
    result = dict(id=f'{path}::{pid}::field:{field}', resourcePath=path,
                  locator=f'MonoBehaviour:{pid}/{script}/{field}', sourceText=text,
                  context=f'{script} / {name} / {field}', speaker=None, kind=kind)
    protected = []
    if field == 'description':
        if script == 'Effect_System.Trigger_System.Trigger_Mechanisms.PeriodicallyTrigger':
            protected = ['(rate)', '(actionName)', '(damage)', '(br)']
        elif script == 'Effect_System.Effects.StatConversionEffect':
            protected = ['(current)']
    protected = [token for token in protected if token in text]
    if protected:
        result['protectedTokens'] = protected
    return result


class TextFields:
    def __init__(self, raw):
        self.raw = raw
        self.tokens = []

    def rebuild(self, translations):
        chunks, position = [], 0
        for start, end, entry in self.tokens:
            text = translations.get(entry['id'], entry['sourceText'])
            if not isinstance(text, str):
                raise ValueError('译文必须是字符串')
            if text != entry['sourceText']:
                chunks.extend((self.raw[position:start], unity_string(text)))
                position = end
        return b''.join(chunks) + self.raw[position:] if chunks else self.raw


class Dialogue(TextFields):
    def __init__(self, raw, path, pid):
        super().__init__(raw)
        r = Reader(raw)
        name = r.string()
        self.active = name.startswith('Dialogue_') and len(name) >= 10
        if not self.active:
            return
        size = r.i32()
        self.json_start = r.pos
        data = r.take(size)
        r.align()
        self.json_end = r.pos
        self.json = data
        text = data.decode('utf-8-sig')
        bom = 3 if data.startswith(b'\xef\xbb\xbf') else 0
        decoder = json.JSONDecoder()
        position = 0
        def whitespace():
            nonlocal position
            while position < len(text) and text[position] in ' \t\r\n':
                position += 1
        whitespace()
        if text[position:position + 1] != '{':
            raise ValueError('对话表必须为 JSON 对象')
        position += 1
        seen = set()
        while True:
            whitespace()
            if text[position:position + 1] == '}':
                position += 1
                break
            key, position = decoder.raw_decode(text, position)
            if not isinstance(key, str) or key in seen:
                raise ValueError('对话表键重复或无效')
            seen.add(key)
            whitespace()
            if text[position:position + 1] != ':':
                raise ValueError('对话表缺少冒号')
            position += 1
            whitespace()
            start = position
            value, position = decoder.raw_decode(text, position)
            if not isinstance(value, str):
                raise ValueError('对话表必须使用字符串值')
            a = bom + len(text[:start].encode('utf-8'))
            b = bom + len(text[:position].encode('utf-8'))
            self.tokens.append((a, b, unit(path, pid, key, value, name)))
            whitespace()
            if text[position:position + 1] == ',':
                position += 1
                whitespace()
                if text[position:position + 1] == '}':
                    raise ValueError('对话表末尾逗号无效')
            elif text[position:position + 1] != '}':
                raise ValueError('对话表结构无效')
        whitespace()
        if position != len(text):
            raise ValueError('对话表存在额外内容')

    def rebuild(self, translations):
        if not self.active:
            return self.raw
        chunks, position = [], 0
        for start, end, entry in self.tokens:
            value = translations.get(entry['id'], entry['sourceText'])
            if not isinstance(value, str):
                raise ValueError('译文必须是字符串')
            if value != entry['sourceText']:
                encoded = json.dumps(value, ensure_ascii=False, separators=(',', ':')).encode('utf-8')
                chunks.extend((self.json[position:start], encoded))
                position = end
        if not chunks:
            return self.raw
        data = b''.join(chunks) + self.json[position:]
        import struct
        return (self.raw[:self.json_start - 4] + struct.pack('<i', len(data)) + data
                + bytes((-len(data)) % 4) + self.raw[self.json_end:])


def serialized(raw, path, pid, script, config):
    skill = script.startswith('Skill_System.Skill_Behaviours.') and script.rsplit('.', 1)[-1] in config['skills']
    ability = script.startswith('Player.Player_Class_Ability.') and script.rsplit('.', 1)[-1] in config['abilities']
    status = script.startswith('Combat.Status_Effects.') and script.rsplit('.', 1)[-1] in config['statuses']
    if not (skill or ability or status or script in config['supported']):
        return None
    result = TextFields(raw)
    r = Reader(raw, 28)
    name = r.string()
    def text(field, kind):
        start = r.pos
        value = r.string()
        if not value.strip():
            return
        if script == 'TMPro.TextMeshProUGUI' and ('NullReferenceException' in value or value.strip() == 'sadasdasd'):
            return
        if script == 'Rewards.Reward' and field == 'description' and value.replace('\r\n', '\n') == 'NOT AVAILABLE\n(Tell dev?)':
            return
        result.tokens.append((start, r.pos, unit(path, pid, field, value, name, script, kind)))
    def texts(kind, *fields):
        for field in fields:
            text(field, kind)
    def finish():
        if r.pos != len(raw):
            raise ValueError('序列化布局存在未识别字段：' + script)
    if skill:
        r.array(8); text('skillName', 'skill'); r.skip(12)
        texts('skill', 'description', 'shortDescription')
    elif ability:
        texts('skill', 'title', 'description')
        if script in ('Player.Player_Class_Ability.DemonHuntingClassAbility', 'Player.Player_Class_Ability.PirateBountyClassAbility'):
            r.skip(16); r.array(8); r.array(8)
            for i in range(r.count(20)):
                encounter = script.endswith('DemonHuntingClassAbility')
                text(f'encounters[{i}].Text' if encounter else f'pirateEffects[{i}].Title', 'dialogue' if encounter else 'skill')
                r.skip(16)
            finish()
    elif status:
        text('displayName', 'skill'); r.skip(4); r.boolean(); r.skip(40)
        for _ in range(8): r.boolean()
        r.skip(12); text('worldTextOnApply', 'system'); r.array(8)
        kind = script.rsplit('.', 1)[-1]
        if kind in ('DurationBasedBuffDebuffStatusEffect', 'StackAbleBuffDebuffStatusEffect', 'RoomBasedBuffDebuffStatusEffect'):
            r.skip(4); r.boolean(); r.boolean(); texts('skill', 'activeDescription', 'informativeDescription')
        elif kind == 'PermanentBuffDebuffStatusEffect':
            r.array(8); texts('skill', 'activeDescription', 'informativeDescription', 'durationDescription')
        elif kind == 'StackAblePermanentBuffDebuffStatusEffect':
            texts('skill', 'activeDescription', 'informativeDescription', 'durationDescription')
            r.skip(4); r.boolean(); r.array(8); r.boolean(); text('onRemoveWorldText', 'system')
        elif kind == 'PermanentPlayerEffectStatusEffect': texts('skill', 'description', 'durationDescription')
        elif kind == 'RoomBasedPlayerEffectStatusEffect':
            r.skip(4); r.boolean(); r.boolean(); text('description', 'skill')
    elif script == 'TMPro.TextMeshProUGUI': r.skip(56); text('m_text', 'ui')
    elif script == 'Combat.Equipment.Equipment': r.skip(36); texts('item', 'itemName', 'description')
    elif script == 'Weapons.Weapon': r.skip(52); r.array(12); text('weaponName', 'item')
    elif script == 'Player.Collection_Items.CollectionItem':
        if config.get('collectionAlignedBool'): r.boolean()
        r.skip(36); texts('item', 'title', 'description')
    elif script == 'Stats.SimpleStat': r.skip(4); texts('skill', 'activeDescription', 'informativeDescription')
    elif script == 'Stats.Dynamic_Stats.DynamicStat': texts('skill', 'activeDescription', 'informativeDescription')
    elif script == 'Player.Player_Class_Ability.EffectClassAbility': texts('skill', 'title', 'description')
    elif script in ('Effect_System.Trigger_System.Trigger', 'Effect_System.Trigger_System.Trigger_Mechanisms.RoomCompletionTrigger',
                    'Effect_System.Trigger_System.Trigger_Mechanisms.StandingStillTrigger', 'Effect_System.Effects.ConditionalEffect',
                    'Effect_System.Effects.StatConversionEffect'): text('description', 'skill')
    elif script == 'Effect_System.Trigger_System.Trigger_Mechanisms.PeriodicallyTrigger': r.boolean(); text('description', 'skill')
    elif script == 'Effect_System.Effects.AttributeStatEffect':
        texts('skill', 'activeDescription', 'informativeDescription'); text('characterPanelLabel', 'ui')
    elif script == 'Effect_System.Effects.RegisterBonusBuffDebuffEffect': texts('skill', 'activeDescription', 'informativeDescription')
    elif script == 'Effect_System.Effects.GrantOrbitalEffect': text('orbitalName', 'skill')
    elif script == 'Behaviour_Graphs.Components.Triggers.InteractionTrigger': text('interactionName', 'ui')
    elif script in ('Shrines.Shrine', 'Shrines.DruidShrine'): text('shrineName', 'ui')
    elif script == 'Shops.Tavernkeep.TavernNPC': text('npcName', 'name')
    elif script == 'Dungeon_Generation.FloorConfig': text('floorTitle', 'ui')
    elif script == 'Cinder_System.CinderModifier': r.skip(20); texts('ui', 'title', 'description')
    elif script == 'UI.Title_Screen.TitleMenuButton': text('text', 'ui')
    elif script == 'UI.Menus.Inventory.DiceButton':
        for _ in range(6): r.boolean()
        r.array(12); r.skip(32); text('description', 'ui')
    elif script == 'World_Progression.WorldObjective':
        r.skip(4); r.boolean(); r.skip(24)
        texts('ui', 'title', 'mainDescription', 'conditionDescription', 'effectDescription', 'unlockMessage')
    elif script == 'Items.Consumables.Consumable': text('itemName', 'item'); r.skip(20); text('description', 'item')
    elif script == 'Traits.Trait': text('traitName', 'skill'); r.skip(12); text('description', 'skill')
    elif script == 'Gifts.Gift': r.skip(4); texts('item', 'name', 'description')
    elif script == 'Meta_Perk_Tree.System.MetaPerk': r.skip(16); text('description', 'skill')
    elif script == 'Combat.Equipment.SetBonus': texts('item', 'setName', 'description')
    elif script == 'Weapons.Weapon_Enchantments.WeaponEnchantment': text('enchantmentName', 'item')
    elif script == 'Effect_System.Effects.DummyDescriptionEffect': texts('skill', 'description', 'shortDescription')
    elif script == 'Player.PlayerClass': r.skip(8); r.array(12); r.array(12); r.skip(36); text('description', 'ui')
    elif script == 'Rewards.Reward_Effects.AlternateDescriptionFromMetaPerkRewardEffect': r.skip(4); text('alternateDescription', 'ui'); finish()
    elif script == 'Rewards.Reward': r.skip(12); r.boolean(); r.array(12); text('title', 'ui'); r.skip(16); text('description', 'ui')
    return result if result.tokens else None


def collect(unity, config):
    fields = {}
    for path, asset in unity.assets.items():
        for obj in asset.objects.values():
            if obj.class_id == 49:
                field = Dialogue(asset.raw(obj), path, obj.path_id)
                if field.active:
                    fields[path, obj.path_id] = field
            elif obj.class_id == 114 and config['supported']:
                raw = asset.raw(obj)
                script = unity.script(path, raw)
                try:
                    field = serialized(raw, path, obj.path_id, script, config)
                except (ValueError, IndexError) as error:
                    raise ValueError(f'{path}:{obj.path_id}/{script}: {error}') from error
                if field:
                    fields[path, obj.path_id] = field
    return fields
