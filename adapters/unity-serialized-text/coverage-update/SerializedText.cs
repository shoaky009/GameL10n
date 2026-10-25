using System;using System.IO;using System.Linq;using System.Text;using System.Collections.Generic;
// Unity 2022 SerializedFile v22, stripped trees. Only layouts confirmed against
// IL2CPP metadata and real objects are enabled; never scan arbitrary strings.
public sealed record SerializedTextToken(string Field,string Text,string Kind,int Start,int End);
public sealed class SerializedText {
 public long PathId;public string Script="",Name="";public byte[] Raw=Array.Empty<byte>();public List<SerializedTextToken> Tokens=new();
 static readonly HashSet<string> Skills=new(StringComparer.Ordinal){
 "DeceiveSkillBehaviour","RendSkillBehaviour","SantaSkillBehaviour","TownPortalSkillBehaviour","DashingStrikeSkillBehaviour","WildShapeSkillBehaviour","FlexSkillBehaviour","PiercingVolleySkillBehavior","ShieldBashSkillBehaviour","ParrySkillBehaviour","SuperFlightSkillBehaviour","MagicCircleSkillBehaviour","HighNoonSkillBehaviour","BlessSkillBehaviour","ArrestSkillBehaviour","PickpocketSkillBehaviour","CharismaSkillBehaviour","JinxSkillBehaviour","PolymorphSkillBehaviour","JumpSkillBehaviour","SmokeGrenadeSkillBehaviour","FlameWallSkillBehaviour","FrostNovaSkillBehaviour","MimicSkillBehaviour","MissileStormSkillBehaviour","VengeanceSkillBehaviour","TelekineticBarrierSkillBehaviour","RallyRiffraffSkillBehaviour","DarkArtsSkillBehaviour","SummonLichSkillBehaviour","HeroicFlagSkillBehaviour","SerenitySkillBehaviour","TwirlSkillBehaviour","HolyShieldSkillBehaviour"};
 static bool IsSkill(string s)=>s.StartsWith("Skill_System.Skill_Behaviours.",StringComparison.Ordinal)&&Skills.Contains(s.Substring("Skill_System.Skill_Behaviours.".Length));
 static readonly HashSet<string> ClassAbilities=new(StringComparer.Ordinal){"AlchemyClassAbility","CopyRandomOtherClassAbility","DemonHuntingClassAbility","DoppelgangerClassAbility","DruidClassAbility","EffectClassAbility","EveryFloorHasEventRoomClassAbility","PirateBountyClassAbility","PsychicTraitClassAbility","SuperTraitClassAbility"};
 static readonly HashSet<string> StatusEffects=new(StringComparer.Ordinal){"AttributeGrowthStatusEffect","AuraStatusEffect","BreakArmorOnStacksReachedStatusEffect","DamageOverTimeStatusEffect","DealDamageAfterNotRefreshingStatusEffect","DealDamageOnDamageTakenStatusEffect","DealDamageOnStacksReachedStatusEffect","DurationBasedBuffDebuffStatusEffect","DurationBasedPlayerEffectStatusEffect","IncreaseCurseStatusEffect","KillOnReachMaxStacks","PermanentBuffDebuffStatusEffect","PermanentPlayerEffectStatusEffect","RoomBasedBuffDebuffStatusEffect","RoomBasedPlayerEffectStatusEffect","SpawnProjectileOnActorDeathStatusEffect","StackAbleBuffDebuffStatusEffect","StackAblePermanentBuffDebuffStatusEffect","StunnedStatusEffect","TipsinessStatusEffect"};
 static bool IsAbility(string s)=>s.StartsWith("Player.Player_Class_Ability.",StringComparison.Ordinal)&&ClassAbilities.Contains(s.Substring("Player.Player_Class_Ability.".Length));
 static bool IsStatus(string s)=>s.StartsWith("Combat.Status_Effects.",StringComparison.Ordinal)&&StatusEffects.Contains(s.Substring("Combat.Status_Effects.".Length));
 public static bool Supported(string s)=>IsSkill(s)||IsAbility(s)||IsStatus(s)||s is "TMPro.TextMeshProUGUI" or "Combat.Equipment.Equipment" or "Weapons.Weapon" or "Player.Collection_Items.CollectionItem" or "Items.Consumables.Consumable" or "Traits.Trait" or "Gifts.Gift" or "Meta_Perk_Tree.System.MetaPerk" or "Combat.Equipment.SetBonus" or "Weapons.Weapon_Enchantments.WeaponEnchantment" or "Effect_System.Effects.DummyDescriptionEffect" or "Player.PlayerClass" or "Rewards.Reward_Effects.AlternateDescriptionFromMetaPerkRewardEffect" or "Rewards.Reward" or "Stats.SimpleStat" or "Stats.Dynamic_Stats.DynamicStat" or "World_Progression.WorldObjective"
  or "Effect_System.Trigger_System.Trigger" or "Effect_System.Trigger_System.Trigger_Mechanisms.PeriodicallyTrigger" or "Effect_System.Trigger_System.Trigger_Mechanisms.RoomCompletionTrigger" or "Effect_System.Trigger_System.Trigger_Mechanisms.StandingStillTrigger"
  or "Effect_System.Effects.AttributeStatEffect" or "Effect_System.Effects.RegisterBonusBuffDebuffEffect" or "Effect_System.Effects.StatConversionEffect" or "Effect_System.Effects.ConditionalEffect" or "Effect_System.Effects.GrantOrbitalEffect"
  or "Behaviour_Graphs.Components.Triggers.InteractionTrigger" or "Shrines.Shrine" or "Shrines.DruidShrine" or "Shops.Tavernkeep.TavernNPC" or "Cinder_System.CinderModifier" or "Dungeon_Generation.FloorConfig" or "UI.Title_Screen.TitleMenuButton" or "UI.Menus.Inventory.DiceButton";
 public static string[]? ProtectedTokens(string script,string field,string text){
  string[] names=script switch{
   "Effect_System.Trigger_System.Trigger_Mechanisms.PeriodicallyTrigger" when field=="description"=>new[]{"(rate)","(actionName)","(damage)","(br)"},
   "Effect_System.Effects.StatConversionEffect" when field=="description"=>new[]{"(current)"},
   _=>Array.Empty<string>()};
  var result=names.Where(n=>text.Contains(n,StringComparison.Ordinal)).ToArray();return result.Length==0?null:result;
 }
 public static string Id(string path,long pid,string field)=>path+"::"+pid+"::field:"+field;
 public static SerializedText? Parse(UnitySet set,string path,dynamic inf){if((int)inf.TypeId!=114)return null;byte[] raw=set.Assets[path].Raw(inf);string s=set.Script(path,raw);return Supported(s)?Parse(raw,(long)inf.PathId,s):null;}
 public static SerializedText? Parse(byte[] raw,long pid,string script){
  if(!Supported(script))return null;var a=new SerializedText{Raw=raw,PathId=pid,Script=script};using var r=new BinaryReader(new MemoryStream(raw));
  void Skip(int n){if(n<0||r.BaseStream.Position+n>raw.Length)throw new InvalidDataException("序列化字段越界："+script+":"+pid);r.BaseStream.Position+=n;}
  void AlignedBool(){if(r.ReadInt32() is not (0 or 1))throw new InvalidDataException("序列化布尔字段不匹配："+script+":"+pid);}
  void Array(int stride){int n=r.ReadInt32();if(n<0||(long)n*stride>raw.Length-r.BaseStream.Position)throw new InvalidDataException("序列化数组越界："+script+":"+pid);Skip(checked(n*stride));}
  void Text(string field,string kind){int start=(int)r.BaseStream.Position;string text=UnityResources.ReadString(r);int end=(int)r.BaseStream.Position;if(end>raw.Length)throw new InvalidDataException("序列化字符串填充越界");if(string.IsNullOrWhiteSpace(text))return;if(script=="TMPro.TextMeshProUGUI"&&(text.Contains("NullReferenceException",StringComparison.Ordinal)||text.Trim()=="sadasdasd"))return;if(script=="Rewards.Reward"&&field=="description"&&text.Replace("\r\n","\n")=="NOT AVAILABLE\n(Tell dev?)")return;a.Tokens.Add(new(field,text,kind,start,end));}
  Skip(28);a.Name=UnityResources.ReadString(r);
  // WeightEffect is a serialized struct: Action + Metric (two 32-bit enums).
  if(IsSkill(script)){Array(8);Text("skillName","skill");Skip(12);Text("description","skill");Text("shortDescription","skill");}
  else if(IsAbility(script)){
   Text("title","skill");Text("description","skill");
   if(script is "Player.Player_Class_Ability.DemonHuntingClassAbility" or "Player.Player_Class_Ability.PirateBountyClassAbility"){
    Skip(16);Array(8);Array(8); // Color, WeightEffect structs, managed-reference tooltip IDs.
    int n=r.ReadInt32();if(n<0||n>(raw.Length-r.BaseStream.Position)/20)throw new InvalidDataException("职业能力文字数组越界");
    for(int i=0;i<n;i++){
     bool encounter=script.EndsWith("DemonHuntingClassAbility",StringComparison.Ordinal);
     Text((encounter?"encounters[":"pirateEffects[")+i+(encounter?"].Text":"].Title"),encounter?"dialogue":"skill");
     Skip(16); // TaintedEncounter: int + PPtr; PirateEffect: PPtr + int.
    }
    if(r.BaseStream.Position!=raw.Length)throw new InvalidDataException("职业能力数组布局不受支持");
   }
  }
  else if(IsStatus(script)){
   Text("displayName","skill");Skip(4);AlignedBool();Skip(12+16+12);
   for(int i=0;i<8;i++)AlignedBool();
   Skip(12);Text("worldTextOnApply","system");Array(8);
   // Only these verified derived layouts contain serialized description templates.
   // Key/type enums and the managed-reference registry remain byte-for-byte intact.
   string kind=script.Substring("Combat.Status_Effects.".Length);
   switch(kind){
    case "DurationBasedBuffDebuffStatusEffect":case "StackAbleBuffDebuffStatusEffect":
    case "RoomBasedBuffDebuffStatusEffect":
     Skip(4);AlignedBool();AlignedBool();Text("activeDescription","skill");Text("informativeDescription","skill");break;
    case "PermanentBuffDebuffStatusEffect":
     Array(8);Text("activeDescription","skill");Text("informativeDescription","skill");Text("durationDescription","skill");break;
    case "StackAblePermanentBuffDebuffStatusEffect":
     Text("activeDescription","skill");Text("informativeDescription","skill");Text("durationDescription","skill");
     Skip(4);AlignedBool();Array(8);AlignedBool();Text("onRemoveWorldText","system");break;
    case "PermanentPlayerEffectStatusEffect":Text("description","skill");Text("durationDescription","skill");break;
    case "RoomBasedPlayerEffectStatusEffect":Skip(4);AlignedBool();AlignedBool();Text("description","skill");break;
   }
  }
  else switch(script){
   case "TMPro.TextMeshProUGUI":Skip(56);Text("m_text","ui");break;
   case "Combat.Equipment.Equipment":Skip(36);Text("itemName","item");Text("description","item");break;
   case "Weapons.Weapon":Skip(52);Array(12);Text("weaponName","item");break;
   case "Player.Collection_Items.CollectionItem":AlignedBool();Skip(36);Text("title","item");Text("description","item");break;
   // Base-class fields precede derived fields. Dynamic values are substituted
   // inside these serialized templates; keys and the variable tail stay untouched.
   case "Stats.SimpleStat":Skip(4);Text("activeDescription","skill");Text("informativeDescription","skill");break;
   case "Stats.Dynamic_Stats.DynamicStat":Text("activeDescription","skill");Text("informativeDescription","skill");break;
   case "Effect_System.Trigger_System.Trigger":
   case "Effect_System.Trigger_System.Trigger_Mechanisms.RoomCompletionTrigger":
   case "Effect_System.Trigger_System.Trigger_Mechanisms.StandingStillTrigger":
   case "Effect_System.Effects.ConditionalEffect":case "Effect_System.Effects.StatConversionEffect":Text("description","skill");break;
   case "Effect_System.Trigger_System.Trigger_Mechanisms.PeriodicallyTrigger":AlignedBool();Text("description","skill");break;
   case "Effect_System.Effects.AttributeStatEffect":Text("activeDescription","skill");Text("informativeDescription","skill");Text("characterPanelLabel","ui");break;
   case "Effect_System.Effects.RegisterBonusBuffDebuffEffect":Text("activeDescription","skill");Text("informativeDescription","skill");break;
   case "Effect_System.Effects.GrantOrbitalEffect":Text("orbitalName","skill");break;
   case "Behaviour_Graphs.Components.Triggers.InteractionTrigger":Text("interactionName","ui");break;
   case "Shrines.Shrine":case "Shrines.DruidShrine":Text("shrineName","ui");break;
   case "Shops.Tavernkeep.TavernNPC":Text("npcName","name");break;
   case "Dungeon_Generation.FloorConfig":Text("floorTitle","ui");break;
   case "Cinder_System.CinderModifier":Skip(20);Text("title","ui");Text("description","ui");break;
   case "UI.Title_Screen.TitleMenuButton":Text("text","ui");break;
   case "UI.Menus.Inventory.DiceButton":for(int i=0;i<6;i++)AlignedBool();Array(12);Skip(12+4+16);Text("description","ui");break;
   case "World_Progression.WorldObjective":Skip(4);AlignedBool();Skip(24);Text("title","ui");Text("mainDescription","ui");Text("conditionDescription","ui");Text("effectDescription","ui");Text("unlockMessage","ui");break;
   case "Items.Consumables.Consumable":Text("itemName","item");Skip(20);Text("description","item");break;
   case "Traits.Trait":Text("traitName","skill");Skip(12);Text("description","skill");break;
   case "Gifts.Gift":Skip(4);Text("name","item");Text("description","item");break;
   case "Meta_Perk_Tree.System.MetaPerk":Skip(16);Text("description","skill");break;
   case "Combat.Equipment.SetBonus":Text("setName","item");Text("description","item");break;
   case "Weapons.Weapon_Enchantments.WeaponEnchantment":Text("enchantmentName","item");break;
   case "Effect_System.Effects.DummyDescriptionEffect":Text("description","skill");Text("shortDescription","skill");break;
   case "Player.PlayerClass":Skip(8);Array(12);Array(12);Skip(16+4+12+4);Text("description","ui");break;
   // RewardEffect has no serialized fields. The enum perkId precedes the
   // description; all 14 real effects are referenced by Rewards.Reward objects.
   case "Rewards.Reward_Effects.AlternateDescriptionFromMetaPerkRewardEffect":Skip(4);Text("alternateDescription","ui");if(r.BaseStream.Position!=raw.Length)throw new InvalidDataException("奖励替代说明布局不受支持");break;
   // preview PPtr, aligned animatedSprite bool, previewFrames Sprite PPtr[].
   // Verified 161 empty arrays and the real 5-frame Set Item Reward, whose
   // frame references resolve to class 213. titleColor is 4 floats (16 bytes).
   // Leave the variable gameplay/managed-reference tail completely untouched.
   case "Rewards.Reward":Skip(12);int animated=r.ReadInt32();if(animated<0||animated>1)throw new InvalidDataException("奖励动画标志不受支持");Array(12);Text("title","ui");Skip(16);Text("description","ui");break;
  }
  return a.Tokens.Count==0?null:a;
 }
 public byte[] Rebuild(string path,IReadOnlyDictionary<string,string> tr){
  var replacements=Tokens.Where(t=>tr.TryGetValue(Id(path,PathId,t.Field),out var text)&&text!=t.Text).ToArray();if(replacements.Length==0)return Raw;
  using var m=new MemoryStream();using var w=new BinaryWriter(m);int pos=0;foreach(var t in replacements){string text=tr[Id(path,PathId,t.Field)]??throw new InvalidDataException("译文不得为 null");_=new UTF8Encoding(false,true).GetBytes(text);w.Write(Raw,pos,t.Start-pos);UnityResources.WriteString(w,text);pos=t.End;}w.Write(Raw,pos,Raw.Length-pos);return m.ToArray();
 }
}
